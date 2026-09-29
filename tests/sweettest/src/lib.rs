//! Sweettest harness for the DEX MVP.
//!
//! Multi-conductor tests of the mock-ledger settlement flow, including the
//! end-to-end demonstration in §30 of the protocol spec.
//!
//! Needs the packed DNA: run `./build.sh` first (or set `DEX_DNA_PATH`).

// Test-only crate: without this, the non-test library build that `cargo test`
// also makes (for doctests) reports every fixture as dead code.
#![cfg(test)]

use dex_api::{BookView, MyOrder, OrderStatus, PriceLevel, RawListing, TakePlan, TakeRequest, TakeResult};
use holochain::prelude::*;
use holochain::conductor::api::error::ConductorApiResult;
use holochain::sweettest::*;
use ledger_api::{
    Amounts, BalanceView, EscrowState, OrderTerms, ParkRequest, ParkStatus, RunEscrowInput,
    RunMode, RunReport, Side,
};
use serde::Serialize;
use std::path::PathBuf;

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

fn dna_path() -> PathBuf {
    std::env::var("DEX_DNA_PATH").map(PathBuf::from).unwrap_or_else(|_| {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../dnas/dex/workdir/dex.dna")
    })
}

/// One agent: their conductor index and cell.
struct Agent {
    conductor: usize,
    cell: SweetCell,
}

struct TestEnv {
    conductors: SweetConductorBatch,
    agents: Vec<Agent>,
}

impl TestEnv {
    async fn new(n: usize) -> Self {
        let dna = SweetDnaFile::from_bundle(&dna_path())
            .await
            .expect("DNA bundle not found: run ./build.sh first");
        let mut conductors = SweetConductorBatch::from_config_rendezvous(n, SweetConductorConfig::standard()).await;
        let apps = conductors
            .setup_app("dex", &[("dex".to_string(), dna)])
            .await
            .unwrap();
        let agents = apps
            .cells_flattened()
            .into_iter()
            .enumerate()
            .map(|(conductor, cell)| Agent { conductor, cell })
            .collect();
        conductors.exchange_peer_info().await;
        Self { conductors, agents }
    }

    async fn zome_call<I, O>(&self, agent: usize, zome: &str, f: &str, input: I) -> ConductorApiResult<O>
    where
        I: Serialize + std::fmt::Debug,
        O: serde::de::DeserializeOwned + std::fmt::Debug,
    {
        let a = &self.agents[agent];
        self.conductors[a.conductor]
            .call_fallible(&a.cell.zome(zome), f, input)
            .await
    }

    /// Call a `ledger` extern, panicking on error.
    async fn call<I, O>(&self, agent: usize, f: &str, input: I) -> O
    where
        I: Serialize + std::fmt::Debug,
        O: serde::de::DeserializeOwned + std::fmt::Debug,
    {
        self.zome_call(agent, "ledger", f, input)
            .await
            .unwrap_or_else(|e| panic!("ledger.{f} failed: {e:?}"))
    }

    async fn call_fallible<I, O>(&self, agent: usize, f: &str, input: I) -> ConductorApiResult<O>
    where
        I: Serialize + std::fmt::Debug,
        O: serde::de::DeserializeOwned + std::fmt::Debug,
    {
        self.zome_call(agent, "ledger", f, input).await
    }

    /// Call a `dex` extern, panicking on error.
    async fn dex<I, O>(&self, agent: usize, f: &str, input: I) -> O
    where
        I: Serialize + std::fmt::Debug,
        O: serde::de::DeserializeOwned + std::fmt::Debug,
    {
        self.zome_call(agent, "dex", f, input)
            .await
            .unwrap_or_else(|e| panic!("dex.{f} failed: {e:?}"))
    }

    async fn dex_fallible<I, O>(&self, agent: usize, f: &str, input: I) -> ConductorApiResult<O>
    where
        I: Serialize + std::fmt::Debug,
        O: serde::de::DeserializeOwned + std::fmt::Debug,
    {
        self.zome_call(agent, "dex", f, input).await
    }

    async fn book(&self, agent: usize) -> BookView {
        self.dex(agent, "get_order_book", ()).await
    }

    /// Σ `BalanceView.total` over every agent. Must equal what was minted.
    async fn total_supply(&self) -> Amounts {
        let mut total = Amounts::ZERO;
        for agent in 0..self.agents.len() {
            total = total.checked_add(self.balance(agent).await.total).unwrap();
        }
        total
    }

    async fn sync(&self) {
        await_consistency(self.agents.iter().map(|a| &a.cell))
            .await
            .unwrap();
    }

    async fn balance(&self, agent: usize) -> BalanceView {
        self.call(agent, "get_balance", ()).await
    }

    async fn collect(&self, agent: usize) {
        let _: Vec<ActionHash> = self.call(agent, "collect_all", ()).await;
    }
}

const ALICE: usize = 0;
const BOB: usize = 1;
const CAROL: usize = 2;

fn in_one_hour() -> i64 {
    Timestamp::now().as_micros() + 3_600_000_000
}

/// SELL 100 UNIT-A at 1.20 UNIT-B.
fn sell_100_at_1_20() -> OrderTerms {
    OrderTerms {
        side: Side::Sell,
        price_per_lot: 120,
        lots: 100,
        expires_at: in_one_hour(),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// §30: Alice sells 100 A at 1.20, Bob buys 40, Alice cancels the other 60.
#[tokio::test(flavor = "multi_thread")]
async fn mvp_end_to_end() {
    let env = TestEnv::new(2).await;

    let _: ActionHash = env.call(ALICE, "mint", Amounts::new(10_000, 0)).await;
    let _: ActionHash = env.call(BOB, "mint", Amounts::new(0, 20_000)).await;

    let escrow: ActionHash = env.call(ALICE, "open_escrow", sell_100_at_1_20()).await;
    let alice = env.balance(ALICE).await;
    assert_eq!(alice.available, Amounts::ZERO);
    assert_eq!(alice.locked_in_escrows, Amounts::new(10_000, 0));
    env.sync().await;

    // Bob parks 48.00 B for 40 A.
    let _: ActionHash = env
        .call(
            BOB,
            "park",
            ParkRequest {
                escrow: escrow.clone(),
                amounts: Amounts::new(0, 4_800),
                requested_lots: 40,
            },
        )
        .await;
    env.sync().await;

    // Alice's client runs the fill.
    let report: Option<RunReport> = env
        .call(
            ALICE,
            "run_escrow",
            RunEscrowInput {
                escrow: escrow.clone(),
                mode: RunMode::Fill,
            },
        )
        .await;
    let report = report.expect("a pending park should produce a run");
    assert_eq!(report.filled_lots, 40);
    assert_eq!(report.locked, Amounts::new(6_000, 0));
    env.sync().await;

    env.collect(ALICE).await;
    env.collect(BOB).await;
    env.sync().await;

    let alice = env.balance(ALICE).await;
    assert_eq!(alice.available, Amounts::new(0, 4_800), "Alice receives 48 B");
    assert_eq!(alice.locked_in_escrows, Amounts::new(6_000, 0), "60 A still escrowed");
    let bob = env.balance(BOB).await;
    assert_eq!(bob.available, Amounts::new(4_000, 15_200), "Bob receives 40 A");
    assert_eq!(bob.parked, Amounts::ZERO);

    let state: EscrowState = env.call(BOB, "get_escrow_state", escrow.clone()).await;
    assert_eq!(state.remaining_lots, 60, "order book shows 60 A at 1.20");

    // Alice cancels: the remaining 60 A is released.
    let release: Option<RunReport> = env
        .call(
            ALICE,
            "run_escrow",
            RunEscrowInput {
                escrow: escrow.clone(),
                mode: RunMode::Release,
            },
        )
        .await;
    assert!(release.is_some());
    env.sync().await;
    env.collect(ALICE).await;

    let alice = env.balance(ALICE).await;
    assert_eq!(alice.available, Amounts::new(6_000, 4_800));
    assert_eq!(alice.locked_in_escrows, Amounts::ZERO);
    let state: EscrowState = env.call(ALICE, "get_escrow_state", escrow).await;
    assert!(state.closed);
    assert_eq!(state.remaining_lots, 0);
}

/// §22: two takers race for the same 100 A. One fills, the other is refunded.
#[tokio::test(flavor = "multi_thread")]
async fn two_takers_cannot_both_fill_the_same_escrow() {
    let env = TestEnv::new(3).await;

    let _: ActionHash = env.call(ALICE, "mint", Amounts::new(10_000, 0)).await;
    let _: ActionHash = env.call(BOB, "mint", Amounts::new(0, 12_000)).await;
    let _: ActionHash = env.call(CAROL, "mint", Amounts::new(0, 12_000)).await;
    let escrow: ActionHash = env.call(ALICE, "open_escrow", sell_100_at_1_20()).await;
    env.sync().await;

    for taker in [BOB, CAROL] {
        let _: ActionHash = env
            .call(
                taker,
                "park",
                ParkRequest {
                    escrow: escrow.clone(),
                    amounts: Amounts::new(0, 12_000),
                    requested_lots: 100,
                },
            )
            .await;
    }
    env.sync().await;

    let report: Option<RunReport> = env
        .call(
            ALICE,
            "run_escrow",
            RunEscrowInput {
                escrow: escrow.clone(),
                mode: RunMode::Fill,
            },
        )
        .await;
    let report = report.unwrap();
    assert_eq!(report.filled_lots, 100, "exactly the escrowed amount is sold");
    assert_eq!(report.locked, Amounts::ZERO);
    env.sync().await;

    for agent in [ALICE, BOB, CAROL] {
        env.collect(agent).await;
    }
    env.sync().await;

    let bob = env.balance(BOB).await.available;
    let carol = env.balance(CAROL).await.available;
    let winners = [bob, carol]
        .iter()
        .filter(|b| **b == Amounts::new(10_000, 0))
        .count();
    let refunded = [bob, carol]
        .iter()
        .filter(|b| **b == Amounts::new(0, 12_000))
        .count();
    assert_eq!((winners, refunded), (1, 1), "one fill, one full refund");
    assert_eq!(env.balance(ALICE).await.available, Amounts::new(0, 12_000));
}

/// Validation rejects debits beyond the author's balance.
#[tokio::test(flavor = "multi_thread")]
async fn cannot_escrow_more_than_the_balance() {
    let env = TestEnv::new(1).await;
    let _: ActionHash = env.call(ALICE, "mint", Amounts::new(5_000, 0)).await;
    let result: ConductorApiResult<ActionHash> = env
        .call_fallible(ALICE, "open_escrow", sell_100_at_1_20())
        .await;
    assert!(result.is_err(), "100 A escrow with only 50 A must be rejected");
}

/// Only the maker may execute their escrow's settlement runs.
#[tokio::test(flavor = "multi_thread")]
async fn only_the_maker_can_run_the_escrow() {
    let env = TestEnv::new(2).await;
    let _: ActionHash = env.call(ALICE, "mint", Amounts::new(10_000, 0)).await;
    let escrow: ActionHash = env.call(ALICE, "open_escrow", sell_100_at_1_20()).await;
    env.sync().await;

    let result: ConductorApiResult<Option<RunReport>> = env
        .call_fallible(
            BOB,
            "run_escrow",
            RunEscrowInput {
                escrow,
                mode: RunMode::Release,
            },
        )
        .await;
    assert!(result.is_err());
}

// ---------------------------------------------------------------------------
// Order book (dex zome)
// ---------------------------------------------------------------------------

fn level(price_per_lot: u64, lots: u64, orders: usize) -> PriceLevel {
    PriceLevel {
        price_per_lot,
        lots,
        orders,
    }
}

fn sell(lots: u64, price_per_lot: u64) -> OrderTerms {
    OrderTerms {
        side: Side::Sell,
        price_per_lot,
        lots,
        expires_at: in_one_hour(),
    }
}

/// A placed order is visible to other agents; cancelling removes it.
#[tokio::test(flavor = "multi_thread")]
async fn placed_order_shows_in_the_book_until_cancelled() {
    let env = TestEnv::new(2).await;
    let _: ActionHash = env.call(ALICE, "mint", Amounts::new(10_000, 0)).await;
    let escrow: ActionHash = env.dex(ALICE, "place_order", sell_100_at_1_20()).await;
    env.sync().await;

    let book = env.book(BOB).await;
    assert_eq!(book.asks, vec![level(120, 100, 1)], "Bob sees 1.20 · 100");
    assert!(book.bids.is_empty());
    assert_eq!(book.spread, None);

    let reports: Vec<RunReport> = env.dex(ALICE, "cancel_order", escrow).await;
    assert_eq!(reports.len(), 1);
    env.sync().await;
    env.collect(ALICE).await;

    assert!(env.book(BOB).await.asks.is_empty(), "a cancelled order leaves the book");
    assert_eq!(env.balance(ALICE).await.available, Amounts::new(10_000, 0));
    let mine: Vec<MyOrder> = env.dex(ALICE, "my_orders", ()).await;
    assert_eq!(mine[0].status, OrderStatus::Cancelled);
}

/// Validation recomputes the listing tag from the escrow's terms.
#[tokio::test(flavor = "multi_thread")]
async fn listing_with_a_tag_that_disagrees_with_the_escrow_is_rejected() {
    let env = TestEnv::new(2).await;
    let _: ActionHash = env.call(ALICE, "mint", Amounts::new(10_000, 0)).await;
    // An escrow opened through the ledger directly is not listed yet.
    let escrow: ActionHash = env.call(ALICE, "open_escrow", sell_100_at_1_20()).await;
    let terms = env.call::<_, EscrowState>(ALICE, "get_escrow_state", escrow.clone()).await.terms;

    let mut cheaper = terms;
    cheaper.price_per_lot = 110;
    let mut other_side = terms;
    other_side.side = Side::Buy;
    let mut later = terms;
    later.expires_at += 1;
    let good = dex_core::listing::encode_tag(&terms);
    let bad_tags = [
        ("price", dex_core::listing::encode_tag(&cheaper)),
        ("side", dex_core::listing::encode_tag(&other_side)),
        ("expiry", dex_core::listing::encode_tag(&later)),
        ("truncated", good[..16].to_vec()),
    ];
    for (what, tag) in bad_tags {
        let result: ConductorApiResult<ActionHash> = env
            .dex_fallible(ALICE, "list_escrow_raw", RawListing { escrow: escrow.clone(), tag })
            .await;
        assert!(result.is_err(), "a listing with the wrong {what} must be rejected");
    }

    // Control: the escrow's own tag passes, so the rejections above are about
    // the tag and the cross-zome escrow decode works.
    let _: ActionHash = env
        .dex(ALICE, "list_escrow_raw", RawListing { escrow: escrow.clone(), tag: good })
        .await;
    env.sync().await;
    assert_eq!(env.book(BOB).await.asks, vec![level(120, 100, 1)]);
}

/// Only an escrow's maker may list it.
#[tokio::test(flavor = "multi_thread")]
async fn only_the_maker_can_list_an_escrow() {
    let env = TestEnv::new(2).await;
    let _: ActionHash = env.call(ALICE, "mint", Amounts::new(10_000, 0)).await;
    let escrow: ActionHash = env.call(ALICE, "open_escrow", sell_100_at_1_20()).await;
    env.sync().await;

    let result: ConductorApiResult<ActionHash> =
        env.dex_fallible(BOB, "republish_listing", escrow.clone()).await;
    assert!(result.is_err(), "Bob cannot list Alice's escrow");
    assert!(env.book(BOB).await.asks.is_empty());

    let _: ActionHash = env.dex(ALICE, "republish_listing", escrow).await;
    env.sync().await;
    assert_eq!(env.book(BOB).await.asks, vec![level(120, 100, 1)]);
}

/// §13: a 60 A buy takes Alice's 40 at 1.20, then 20 of Carol's 35 at 1.21.
/// Each maker settles their own order; balances and supply are conserved.
#[tokio::test(flavor = "multi_thread")]
async fn take_across_two_orders_is_settled_by_each_maker() {
    let env = TestEnv::new(3).await;
    let _: ActionHash = env.call(ALICE, "mint", Amounts::new(4_000, 0)).await;
    let _: ActionHash = env.call(CAROL, "mint", Amounts::new(3_500, 0)).await;
    let _: ActionHash = env.call(BOB, "mint", Amounts::new(0, 20_000)).await;
    let minted = Amounts::new(7_500, 20_000);

    let alice_order: ActionHash = env.dex(ALICE, "place_order", sell(40, 120)).await;
    let carol_order: ActionHash = env.dex(CAROL, "place_order", sell(35, 121)).await;
    env.sync().await;
    assert_eq!(env.book(BOB).await.asks, vec![level(120, 40, 1), level(121, 35, 1)]);

    let request = TakeRequest {
        take: Side::Buy,
        lots: 60,
        limit_price: None,
    };
    let preview: TakePlan<ActionHash> = env.dex(BOB, "plan_take", request.clone()).await;
    let planned: Vec<(ActionHash, u64)> = preview.fills.iter().map(|f| (f.order.clone(), f.lots)).collect();
    assert_eq!(planned, vec![(alice_order.clone(), 40), (carol_order.clone(), 20)]);
    assert_eq!(preview.total_cost, Amounts::new(0, 4_800 + 2_420));

    let taken: TakeResult = env.dex(BOB, "take", request).await;
    assert_eq!(taken.plan, preview, "nothing changed between preview and take");
    assert_eq!(taken.parks.len(), 2);
    env.sync().await;
    assert_eq!(env.total_supply().await, minted, "parking moves funds, never creates them");

    // Each maker's client settles its own order.
    for (maker, filled) in [(ALICE, 40), (CAROL, 20)] {
        let reports: Vec<RunReport> = env.dex(maker, "run_my_orders", ()).await;
        assert_eq!(reports.len(), 1);
        assert_eq!(reports[0].filled_lots, filled);
    }
    env.sync().await;
    for agent in [ALICE, BOB, CAROL] {
        env.collect(agent).await;
    }
    env.sync().await;

    assert_eq!(env.balance(BOB).await.available, Amounts::new(6_000, 20_000 - 7_220));
    assert_eq!(env.balance(ALICE).await.available, Amounts::new(0, 4_800));
    let carol = env.balance(CAROL).await;
    assert_eq!(carol.available, Amounts::new(0, 2_420));
    assert_eq!(carol.locked_in_escrows, Amounts::new(1_500, 0));
    assert_eq!(env.total_supply().await, minted);

    assert_eq!(env.book(BOB).await.asks, vec![level(121, 15, 1)], "Alice's order is gone");
    let alice: Vec<MyOrder> = env.dex(ALICE, "my_orders", ()).await;
    assert_eq!(alice[0].status, OrderStatus::Filled);
    let carol: Vec<MyOrder> = env.dex(CAROL, "my_orders", ()).await;
    assert_eq!(carol[0].status, OrderStatus::Partial);

    // Running again is a no-op.
    let again: Vec<RunReport> = env.dex(ALICE, "run_my_orders", ()).await;
    assert!(again.is_empty());

    let parks: Vec<ParkStatus> = env.dex(BOB, "my_parks", ()).await;
    let fills: Vec<(ActionHash, Option<u64>)> = parks
        .iter()
        .map(|p| (p.escrow.clone(), p.settlement.as_ref().map(|s| s.filled_lots)))
        .collect();
    assert_eq!(fills, vec![(carol_order, Some(20)), (alice_order, Some(40))], "newest first");
}

// ---------------------------------------------------------------------------
// Lifecycle and ledger edge cases
// ---------------------------------------------------------------------------

fn park_request(escrow: &ActionHash, b: u64, lots: u64) -> ParkRequest {
    ParkRequest {
        escrow: escrow.clone(),
        amounts: Amounts::new(0, b),
        requested_lots: lots,
    }
}

async fn collect_all(env: &TestEnv) {
    for agent in 0..env.agents.len() {
        env.collect(agent).await;
    }
    env.sync().await;
}

/// Orders leave the book when fully filled and when they expire. Also: a fill
/// with nothing pending is a no-op, and a park against an expired order is
/// refunded by the maker's release.
#[tokio::test(flavor = "multi_thread")]
async fn orders_leave_the_book_when_filled_or_expired() {
    let env = TestEnv::new(2).await;
    let _: ActionHash = env.call(ALICE, "mint", Amounts::new(2_000, 0)).await;
    let _: ActionHash = env.call(BOB, "mint", Amounts::new(0, 10_000)).await;
    let minted = Amounts::new(2_000, 10_000);

    let lasting: ActionHash = env.dex(ALICE, "place_order", sell(10, 120)).await;
    let expires_at = Timestamp::now().as_micros() + 30_000_000;
    let short: ActionHash = env
        .dex(ALICE, "place_order", OrderTerms { expires_at, ..sell(10, 130) })
        .await;
    env.sync().await;
    assert_eq!(env.book(BOB).await.asks, vec![level(120, 10, 1), level(130, 10, 1)]);

    // Nothing pending: no run is written.
    let none: Vec<RunReport> = env.dex(ALICE, "run_my_orders", ()).await;
    assert!(none.is_empty());
    let fill: Option<RunReport> = env
        .call(ALICE, "run_escrow", RunEscrowInput { escrow: lasting.clone(), mode: RunMode::Fill })
        .await;
    assert!(fill.is_none());

    // Full fill.
    let _: TakeResult = env
        .dex(BOB, "take", TakeRequest { take: Side::Buy, lots: 10, limit_price: Some(120) })
        .await;
    env.sync().await;
    let reports: Vec<RunReport> = env.dex(ALICE, "run_my_orders", ()).await;
    assert_eq!(reports.iter().map(|r| r.filled_lots).collect::<Vec<_>>(), vec![10]);
    env.sync().await;
    assert_eq!(env.book(BOB).await.asks, vec![level(130, 10, 1)], "a filled order leaves the book");

    // Expiry.
    while Timestamp::now().as_micros() <= expires_at + 1_000_000 {
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    }
    assert!(env.book(BOB).await.asks.is_empty(), "an expired order leaves the book");

    // A park against the expired order (a stale client) is accepted and refunded.
    let _: ActionHash = env.call(BOB, "park", park_request(&short, 1_300, 10)).await;
    env.sync().await;
    let reports: Vec<RunReport> = env.dex(ALICE, "run_my_orders", ()).await;
    assert_eq!(reports.len(), 1);
    assert_eq!((reports[0].mode, reports[0].filled_lots), (RunMode::Release, 0));
    env.sync().await;
    collect_all(&env).await;

    assert_eq!(env.balance(BOB).await.available, Amounts::new(1_000, 8_800), "the late park is refunded");
    assert_eq!(env.balance(ALICE).await.available, Amounts::new(1_000, 1_200));
    let statuses: Vec<OrderStatus> = env
        .dex::<_, Vec<MyOrder>>(ALICE, "my_orders", ())
        .await
        .into_iter()
        .map(|o| o.status)
        .collect();
    assert_eq!(statuses, vec![OrderStatus::Expired, OrderStatus::Filled], "newest first");
    let parks: Vec<Option<u64>> = env
        .dex::<_, Vec<ParkStatus>>(BOB, "my_parks", ())
        .await
        .iter()
        .map(|p| p.settlement.as_ref().map(|s| s.filled_lots))
        .collect();
    assert_eq!(parks, vec![Some(0), Some(10)]);
    assert_eq!(env.total_supply().await, minted);
}

/// Three makers at one price: the level lists them oldest first, and a take
/// consumes them in that order, skipping the taker's own order.
#[tokio::test(flavor = "multi_thread")]
async fn orders_at_one_price_fill_in_time_priority() {
    let env = TestEnv::new(3).await;
    for maker in [ALICE, BOB, CAROL] {
        let _: ActionHash = env.call(maker, "mint", Amounts::new(1_000, 0)).await;
    }
    let _: ActionHash = env.call(CAROL, "mint", Amounts::new(0, 5_000)).await;
    let minted = Amounts::new(3_000, 5_000);

    let mut ids = Vec::new();
    for maker in [ALICE, BOB, CAROL] {
        ids.push(env.dex::<_, ActionHash>(maker, "place_order", sell(10, 120)).await);
        env.sync().await;
    }
    assert_eq!(env.book(ALICE).await.asks, vec![level(120, 30, 3)]);
    let at_level: Vec<ActionHash> = env
        .dex::<_, Vec<dex_api::Order>>(
            ALICE,
            "get_level_orders",
            dex_api::LevelQuery { side: Side::Sell, price_per_lot: 120 },
        )
        .await
        .into_iter()
        .map(|o| o.id)
        .collect();
    assert_eq!(at_level, ids, "oldest first");

    let taken: TakeResult = env
        .dex(CAROL, "take", TakeRequest { take: Side::Buy, lots: 15, limit_price: None })
        .await;
    let planned: Vec<(ActionHash, u64)> = taken.parks.iter().map(|p| (p.escrow.clone(), p.lots)).collect();
    assert_eq!(planned, vec![(ids[0].clone(), 10), (ids[1].clone(), 5)], "Carol skips her own order");
    env.sync().await;
    for maker in [ALICE, BOB] {
        let _: Vec<RunReport> = env.dex(maker, "run_my_orders", ()).await;
    }
    env.sync().await;
    collect_all(&env).await;

    assert_eq!(env.balance(CAROL).await.available, Amounts::new(1_500, 5_000 - 1_800));
    assert_eq!(env.book(ALICE).await.asks, vec![level(120, 15, 2)]);
    assert_eq!(env.total_supply().await, minted);
}

/// 21 parks exceed one run's cap of 20. Both `run_my_orders` (fill) and
/// `cancel_order` (release) settle them with two runs in one call.
#[tokio::test(flavor = "multi_thread")]
async fn more_parks_than_one_run_takes_settle_in_one_call() {
    let env = TestEnv::new(2).await;
    let _: ActionHash = env.call(ALICE, "mint", Amounts::new(20_000, 0)).await;
    let _: ActionHash = env.call(BOB, "mint", Amounts::new(0, 60_000)).await;
    let minted = Amounts::new(20_000, 60_000);
    let to_fill: ActionHash = env.dex(ALICE, "place_order", sell(100, 120)).await;
    let to_cancel: ActionHash = env.dex(ALICE, "place_order", sell(100, 125)).await;
    env.sync().await;

    for _ in 0..21 {
        let _: ActionHash = env.call(BOB, "park", park_request(&to_fill, 120, 1)).await;
        let _: ActionHash = env.call(BOB, "park", park_request(&to_cancel, 125, 1)).await;
    }
    env.sync().await;

    let cancelled: Vec<RunReport> = env.dex(ALICE, "cancel_order", to_cancel).await;
    let pending: Vec<usize> = cancelled.iter().map(|r| r.still_pending).collect();
    assert_eq!(pending, vec![1, 0], "20 refunds, then the 21st");
    assert!(cancelled.iter().all(|r| r.filled_lots == 0));

    let filled: Vec<RunReport> = env.dex(ALICE, "run_my_orders", ()).await;
    let lots: Vec<u64> = filled.iter().map(|r| r.filled_lots).collect();
    assert_eq!(lots, vec![20, 1], "only the open order runs; the cancelled one is settled");
    env.sync().await;
    collect_all(&env).await;

    assert_eq!(env.balance(BOB).await.available, Amounts::new(2_100, 60_000 - 21 * 120));
    let alice = env.balance(ALICE).await;
    assert_eq!(alice.available, Amounts::new(10_000, 21 * 120));
    assert_eq!(alice.locked_in_escrows, Amounts::new(7_900, 0));
    let parks: Vec<ParkStatus> = env.dex(BOB, "my_parks", ()).await;
    assert_eq!(parks.len(), 42);
    assert!(parks.iter().all(|p| p.settlement.is_some()), "no park left waiting");
    assert_eq!(env.total_supply().await, minted);
}
