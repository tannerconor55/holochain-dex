//! Taker protection, checkpoints and markets on real conductors (design doc
//! section 9, layer 3). Crafted-chain tests of every attack trace, with
//! backdated timestamps no conductor would write, are in
//! `ledger_integrity/tests/crafted_chains.rs`.
//!
//! Deadline tests run on a DNA with 20 s / 5 s timing, so a park's deadline
//! is 25 s after it is written.

use super::*;
use dex_api::MakerPresence;
use dex_core::properties::UnitDef;
use dex_core::{MarketDef, MarketId};
use ledger_api::{CheckpointView, RawCheckpoint, RawPark, RawReclaim};

/// The window from a park to its deadline on the short-timing DNA.
const WINDOW_US: i64 = 25_000_000;

impl TestEnv {
    async fn reclaim_raw(&self, agent: usize, park: &ActionHash, anchor: &ActionHash) -> ConductorApiResult<ActionHash> {
        self.call_fallible(agent, "reclaim_raw", RawReclaim { park: park.clone(), anchor: anchor.clone() }).await
    }

    async fn parks(&self, agent: usize) -> Vec<ParkStatus> {
        self.dex(agent, "my_parks", ()).await
    }

    async fn presence(&self, agent: usize, order: &ActionHash) -> MakerPresence {
        let mut all: Vec<MakerPresence> = self.dex(agent, "check_makers", vec![order.clone()]).await;
        all.pop().expect("one answer per order")
    }
}

/// Sleep until the wall clock passes `micros`.
async fn wait_until(micros: i64) {
    while Timestamp::now().as_micros() <= micros {
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
    }
}

/// Poll until `agent`'s first park is reclaimable: their view of the maker's
/// agent activity is eventually consistent. Panics after 60 s with the
/// coordinator's reason.
async fn wait_for_reclaimable(env: &TestEnv, agent: usize) {
    let give_up = Timestamp::now().as_micros() + 60_000_000;
    while !env.parks(agent).await[0].reclaimable {
        if Timestamp::now().as_micros() > give_up {
            let park = env.parks(agent).await[0].park.clone();
            let why: ConductorApiResult<ActionHash> = env.call_fallible(agent, "reclaim_park", park).await;
            panic!("the maker's action never made the park reclaimable: {why:?}");
        }
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    }
}

/// `err`'s text contains `what`: the rejection is the one we meant.
fn assert_rejected<T: std::fmt::Debug>(result: ConductorApiResult<T>, what: &str) {
    match result {
        Ok(v) => panic!("expected a rejection ({what}), got {v:?}"),
        Err(e) => {
            let text = format!("{e:?}");
            assert!(text.contains(what), "rejected, but not for {what:?}: {text}");
        }
    }
}

/// Alice sells 40 lots at 1.20; Bob takes them all. Returns (order, park).
async fn alice_sells_bob_takes(env: &TestEnv) -> (ActionHash, ActionHash) {
    let _: ActionHash = env.call(ALICE, "mint", Amounts::new(4_000, 0)).await;
    let _: ActionHash = env.call(BOB, "mint", Amounts::new(0, 4_800)).await;
    let order: ActionHash = env.dex(ALICE, "place_order", place(sell(40, 120))).await;
    env.sync().await;
    let taken: TakeResult = env
        .dex(BOB, "take", TakeRequest { market: None, take: Side::Buy, lots: 40, limit_price: None })
        .await;
    env.sync().await;
    (order, taken.parks[0].park.clone())
}

/// The maker goes offline after Bob parks. Bob sees the presence check fail,
/// cannot reclaim while the maker has written nothing since the deadline
/// (not through the coordinator, and not by citing the maker's last action),
/// then reclaims once the maker is back and writes anything. His balance is
/// restored, a second reclaim is refused, and the maker cannot settle it.
#[tokio::test(flavor = "multi_thread")]
async fn maker_offline_park_is_reclaimed_after_the_deadline() {
    let mut env = TestEnv::with_properties(2, &short_timing()).await;
    let (order, park) = alice_sells_bob_takes(&env).await;
    assert!(env.presence(BOB, &order).await.reachable, "Alice is online");

    let status = env.parks(BOB).await.remove(0);
    assert_eq!(status.park, park);
    assert!(!status.reclaimable && status.settlement.is_none());
    assert_eq!(status.deadline.as_micros(), status.parked_at.as_micros() + WINDOW_US);
    assert_eq!(env.balance(BOB).await.parked, Amounts::new(0, 4_800));

    // Alice goes offline and stays offline past the deadline.
    let alice_conductor = env.agents[ALICE].conductor;
    env.conductors[alice_conductor].shutdown().await;
    let offline = env.presence(BOB, &order).await;
    assert!(!offline.reachable, "Alice is offline: {offline:?}");
    wait_until(status.deadline.as_micros() + 1_000_000).await;

    assert!(!env.parks(BOB).await[0].reclaimable, "no anchor yet");
    let result: ConductorApiResult<ActionHash> = env.call_fallible(BOB, "reclaim_park", park.clone()).await;
    assert_rejected(result, "maker has written nothing since");
    // Validation agrees: Alice's last action (the listing's escrow) predates the deadline.
    assert_rejected(env.reclaim_raw(BOB, &park, &order).await, "deadline");

    // Alice returns and writes anything at all.
    env.conductors[alice_conductor].startup().await;
    env.conductors.exchange_peer_info().await;
    let _: ActionHash = env.call(ALICE, "mint", Amounts::new(1, 0)).await;
    env.sync().await;
    wait_for_reclaimable(&env, BOB).await;
    let _: ActionHash = env.call(BOB, "reclaim_park", park.clone()).await;
    env.sync().await;
    let bob = env.balance(BOB).await;
    assert_eq!((bob.available, bob.parked), (Amounts::new(0, 4_800), Amounts::ZERO), "Bob has his funds back");
    let status = env.parks(BOB).await.remove(0);
    assert!(status.reclaimed.is_some() && !status.reclaimable);

    // No second reclaim, through the coordinator or around it.
    let again: ConductorApiResult<ActionHash> = env.call_fallible(BOB, "reclaim_park", park.clone()).await;
    assert_rejected(again, "already reclaimed");
    let alice_head: Vec<MyOrder> = env.dex(ALICE, "my_orders", ()).await;
    assert!(alice_head[0].pending.is_empty(), "the maker no longer sees the park");

    // The maker cannot settle it: nothing is left for a run to consume.
    let reports: Vec<RunReport> = env.dex(ALICE, "run_my_orders", ()).await;
    assert!(reports.is_empty(), "no run consumes a reclaimed park: {reports:?}");
    assert_eq!(env.balance(ALICE).await.locked_in_escrows, Amounts::new(4_000, 0));
    env.assert_supply().await;
}

/// Validation refuses every reclaim the attack traces rule out, written
/// directly (`reclaim_raw`) so the coordinator's checks cannot hide it: too
/// early (anchor before the deadline), an anchor off the maker's chain, a
/// stranger reclaiming, and a second reclaim.
#[tokio::test(flavor = "multi_thread")]
async fn validation_refuses_early_foreign_and_repeated_reclaims() {
    let env = TestEnv::with_properties(3, &short_timing()).await;
    let (_order, park) = alice_sells_bob_takes(&env).await;

    // Early: Alice acts at once, well inside the 25 s window.
    let early: ActionHash = env.call(ALICE, "mint", Amounts::new(1, 0)).await;
    let early_at = Timestamp::now().as_micros();
    let deadline = env.parks(BOB).await[0].deadline.as_micros();
    assert!(early_at < deadline, "setup too slow: the early anchor came after the deadline");
    env.sync().await;
    assert_rejected(env.reclaim_raw(BOB, &park, &early).await, "deadline");

    wait_until(deadline + 1_000_000).await;
    let anchor: ActionHash = env.call(ALICE, "mint", Amounts::new(1, 0)).await;
    let carol_action: ActionHash = env.call(CAROL, "mint", Amounts::new(0, 1)).await;
    env.sync().await;

    assert_rejected(env.reclaim_raw(BOB, &park, &carol_action).await, "maker's chain");
    assert_rejected(env.reclaim_raw(CAROL, &park, &anchor).await, "park's author");
    let _: ActionHash = env.reclaim_raw(BOB, &park, &anchor).await.expect("the one valid reclaim");
    assert_rejected(env.reclaim_raw(BOB, &park, &anchor).await, "already reclaimed");
    env.assert_supply().await;
}

/// A settled park cannot be reclaimed, even after the deadline with an
/// anchor; its sibling, never settled, can.
#[tokio::test(flavor = "multi_thread")]
async fn a_settled_park_cannot_be_reclaimed() {
    let env = TestEnv::with_properties(2, &short_timing()).await;
    let _: ActionHash = env.call(ALICE, "mint", Amounts::new(4_000, 0)).await;
    let _: ActionHash = env.call(BOB, "mint", Amounts::new(0, 4_800)).await;
    let order: ActionHash = env.dex(ALICE, "place_order", place(sell(40, 120))).await;
    env.sync().await;

    let settled: ActionHash = env.call(BOB, "park", park_request(&order, 1_200, 10)).await;
    env.sync().await;
    let reports: Vec<RunReport> = env.dex(ALICE, "run_my_orders", ()).await;
    assert_eq!(reports.iter().map(|r| r.filled_lots).sum::<u64>(), 10, "settled in time");
    env.sync().await;
    let waiting: ActionHash = env.call(BOB, "park", park_request(&order, 1_200, 10)).await;
    env.sync().await;

    let deadline = env.parks(BOB).await.iter().map(|p| p.deadline.as_micros()).max().expect("two parks");
    wait_until(deadline + 1_000_000).await;
    let anchor: ActionHash = env.call(ALICE, "mint", Amounts::new(1, 0)).await;
    env.sync().await;

    assert_rejected(env.reclaim_raw(BOB, &settled, &anchor).await, "consumed");
    let refused: ConductorApiResult<ActionHash> = env.call_fallible(BOB, "reclaim_park", settled).await;
    assert_rejected(refused, "settled by the maker");
    let _: ActionHash = env.call(BOB, "reclaim_park", waiting).await;
    collect_all(&env).await;
    assert_eq!(env.balance(BOB).await.available, Amounts::new(1_000, 4_800 - 1_200));
    env.assert_supply().await;
}

// ---------------------------------------------------------------------------
// Checkpoints
// ---------------------------------------------------------------------------

/// A chain of more than 200 ledger entries: checkpoints are written as it
/// grows, balances stay exact, a debit still validates (its walk stops at the
/// latest checkpoint), and a peer validates the whole chain. A checkpoint
/// with a forged total is refused while the true one passes, and citing a
/// superseded checkpoint is refused while citing the latest passes.
#[tokio::test(flavor = "multi_thread")]
async fn long_chains_checkpoint_and_refuse_forged_or_stale_checkpoints() {
    const MINTS: u64 = 210;
    let env = TestEnv::new(2).await;
    for _ in 0..MINTS {
        let _: ActionHash = env.call(ALICE, "mint", Amounts::new(100, 0)).await;
    }
    let total = Amounts::new(100 * MINTS, 0);
    assert_eq!(env.balance(ALICE).await.available, total);

    let checkpoints: Vec<CheckpointView> = env.call(ALICE, "get_my_checkpoints", ()).await;
    assert!(checkpoints.len() >= (MINTS as usize) / 32, "one per 32 entries: {}", checkpoints.len());
    let minted: Vec<u64> = checkpoints.iter().map(|c| c.state.totals.minted.get(dex_core::UNIT_A)).collect();
    assert!(minted.windows(2).all(|w| w[0] < w[1]), "each checkpoint carries more: {minted:?}");

    // A debit after 200+ entries.
    let order: ActionHash = env.dex(ALICE, "place_order", place(sell(100, 120))).await;
    env.sync().await; // Bob validates every op of Alice's chain
    let state: EscrowState = env.call(BOB, "get_escrow_state", order.clone()).await;
    assert_eq!(state.remaining_lots, 100);

    // Forged vs true checkpoint. Alice's chain since her latest checkpoint
    // holds mints and one escrow, so the true state is computable here.
    let checkpoints: Vec<CheckpointView> = env.call(ALICE, "get_my_checkpoints", ()).await;
    let latest = checkpoints.last().expect("checkpoints").clone();
    let mut truth = latest.state.clone();
    truth.totals.minted = total.clone();
    truth.totals.escrowed = Amounts::new(10_000, 0);
    let mut forged = truth.clone();
    forged.totals.minted = total.checked_add(&Amounts::new(1, 0)).unwrap();
    let result: ConductorApiResult<ActionHash> = env
        .call_fallible(ALICE, "checkpoint_raw", RawCheckpoint { prev: Some(latest.checkpoint.clone()), state: forged })
        .await;
    assert_rejected(result, "checkpoint totals do not equal");
    let newest: ActionHash = env
        .call(ALICE, "checkpoint_raw", RawCheckpoint { prev: Some(latest.checkpoint.clone()), state: truth })
        .await;

    // Stale vs latest citation, on a park against Bob's order.
    let _: ActionHash = env.call(BOB, "mint", Amounts::new(0, 1_200)).await;
    let bob_order: ActionHash = env.dex(BOB, "place_order", place(buy(1, 120))).await;
    env.sync().await;
    let raw = |checkpoint: &ActionHash| RawPark {
        escrow: bob_order.clone(),
        market: demo_market(),
        amounts: Amounts::new(100, 0),
        requested_lots: 1,
        checkpoint: Some(checkpoint.clone()),
    };
    let stale: ConductorApiResult<ActionHash> = env.call_fallible(ALICE, "park_raw", raw(&latest.checkpoint)).await;
    assert_rejected(stale, "a newer checkpoint exists");
    let _: ActionHash = env.call(ALICE, "park_raw", raw(&newest)).await;
    env.assert_supply().await;
}

// ---------------------------------------------------------------------------
// Markets
// ---------------------------------------------------------------------------

/// A second market, B/HF, with a 0.05 HF tick.
fn b_market() -> MarketDef {
    MarketDef { base: "B".into(), quote: dex_core::HUB_UNIT.into(), lot_size: 100, tick_size: 5 }
}

fn two_markets() -> DexProperties {
    let mut props = DexProperties::demo();
    props.units.push(UnitDef { id: "B".into(), decimals: 2 });
    props.markets.push(b_market());
    props
}

/// Each market has its own book; a park must be in its escrow's market; a
/// price off the market's tick is refused.
#[tokio::test(flavor = "multi_thread")]
async fn markets_are_separate_books_with_their_own_rules() {
    let env = TestEnv::with_properties(2, &two_markets()).await;
    let b: MarketId = b_market().id();
    let _: ActionHash = env.call(ALICE, "mint", Amounts::of("B", 1_000)).await;
    let _: ActionHash = env.call(BOB, "mint", Amounts::new(0, 5_000)).await;

    let config: dex_api::DexConfig = env.dex(BOB, "get_config", ()).await;
    assert_eq!(config.markets.iter().map(|m| m.id).collect::<Vec<_>>(), vec![demo_market(), b]);

    let off_tick = dex_api::PlaceOrderRequest { market: Some(b), terms: sell(10, 123) };
    let refused: ConductorApiResult<ActionHash> = env.dex_fallible(ALICE, "place_order", off_tick).await;
    assert_rejected(refused, "tick");
    let order: ActionHash = env
        .dex(ALICE, "place_order", dex_api::PlaceOrderRequest { market: Some(b), terms: sell(10, 125) })
        .await;
    env.sync().await;

    assert!(env.book(BOB).await.asks.is_empty(), "the default market's book does not show B/HF");
    let b_book: BookView = env.dex(BOB, "get_order_book", Some(b)).await;
    assert_eq!(b_book.asks, vec![level(125, 10, 1)]);

    let raw = |market: MarketId| RawPark {
        escrow: order.clone(),
        market,
        amounts: Amounts::new(0, 1_250),
        requested_lots: 10,
        checkpoint: None,
    };
    let cross: ConductorApiResult<ActionHash> = env.call_fallible(BOB, "park_raw", raw(demo_market())).await;
    assert_rejected(cross, "in its escrow's market");
    let _: ActionHash = env.call(BOB, "park_raw", raw(b)).await;
    env.sync().await;
    let reports: Vec<RunReport> = env.dex(ALICE, "run_my_orders", ()).await;
    assert_eq!(reports.iter().map(|r| r.filled_lots).sum::<u64>(), 10);
    env.sync().await;
    collect_all(&env).await;
    assert_eq!(env.balance(BOB).await.available, Amounts::of("B", 1_000).checked_add(&Amounts::new(0, 3_750)).unwrap());
    env.assert_supply().await;
}

/// A DNA declaring one pair twice cannot be joined: genesis refuses it.
#[tokio::test(flavor = "multi_thread")]
async fn a_duplicate_market_is_refused_at_genesis() {
    let mut props = DexProperties::demo();
    props.markets.push(MarketDef::default_pair());
    let dna = dna_with(&props).await;
    let mut conductors = SweetConductorBatch::from_config_rendezvous(1, SweetConductorConfig::standard()).await;
    let result = conductors.setup_app("dex", &[("dex".to_string(), dna)]).await;
    let err = match result {
        Ok(_) => panic!("installing a DNA with a duplicate market must fail"),
        Err(e) => format!("{e:?}"),
    };
    assert!(err.contains("declared twice"), "refused for another reason: {err}");
}
