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
use dex_core::properties::DexProperties;
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
    /// Everything minted so far, from successful `mint` calls.
    minted: std::sync::Mutex<Amounts>,
}

/// The packed DNA with its properties replaced by `props`: a different DNA
/// hash, so its own network.
async fn dna_with(props: &DexProperties) -> DnaFile {
    let bytes = ExternIO::encode(props).expect("properties encode").0;
    let modifiers = DnaModifiersOpt { network_seed: None, properties: Some(SerializedBytes::from(UnsafeBytes::from(bytes))) };
    SweetDnaFile::from_bundle_with_overrides(&dna_path(), modifiers)
        .await
        .expect("DNA bundle not found: run ./build.sh first")
}

/// The production properties with the test timing: 20 s to settle a park,
/// then 5 s grace (design doc section 3).
fn short_timing() -> DexProperties {
    DexProperties { park_timeout_secs: 20, settle_grace_secs: 5, ..DexProperties::demo() }
}

impl TestEnv {
    /// `n` agents on the packed DNA as built (production properties).
    async fn new(n: usize) -> Self {
        let dna = SweetDnaFile::from_bundle(&dna_path())
            .await
            .expect("DNA bundle not found: run ./build.sh first");
        Self::with_dna(n, dna).await
    }

    /// `n` agents on the DNA with `props`.
    async fn with_properties(n: usize, props: &DexProperties) -> Self {
        Self::with_dna(n, dna_with(props).await).await
    }

    async fn with_dna(n: usize, dna: DnaFile) -> Self {
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
        Self { conductors, agents, minted: std::sync::Mutex::new(Amounts::ZERO) }
    }

    async fn zome_call<I, O>(&self, agent: usize, zome: &str, f: &str, input: I) -> ConductorApiResult<O>
    where
        I: Serialize + std::fmt::Debug,
        O: serde::de::DeserializeOwned + std::fmt::Debug,
    {
        let a = &self.agents[agent];
        // A mint's input is its amounts: keep count for `assert_supply`.
        let mint = (zome == "ledger" && f == "mint").then(|| ExternIO::encode(&input).ok()).flatten();
        let result = self.conductors[a.conductor].call_fallible(&a.cell.zome(zome), f, input).await;
        if let (Ok(_), Some(io)) = (&result, mint) {
            let amounts: Amounts = io.decode().expect("a mint's input is Amounts");
            let mut minted = self.minted.lock().expect("not poisoned");
            *minted = minted.checked_add(&amounts).expect("no overflow");
        }
        result
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
        self.dex(agent, "get_order_book", None::<dex_core::MarketId>).await
    }

    /// Σ `BalanceView.total` over every agent. Must equal what was minted.
    async fn total_supply(&self) -> Amounts {
        let mut total = Amounts::ZERO;
        for agent in 0..self.agents.len() {
            total = total.checked_add(&self.balance(agent).await.total).unwrap();
        }
        total
    }

    /// Supply is conserved: after syncing, Σ every agent's total equals
    /// everything minted. Run after every scenario.
    async fn assert_supply(&self) {
        self.sync().await;
        let minted = self.minted.lock().expect("not poisoned").clone();
        assert_eq!(self.total_supply().await, minted, "total supply must equal what was minted");
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

/// The demo market the DNA properties declare (A/HF).
fn demo_market() -> dex_core::MarketId {
    dex_core::MarketDef::default_pair().id()
}

/// Open an escrow in the demo market through the ledger directly.
/// Place an order in the default market.
fn place(terms: OrderTerms) -> dex_api::PlaceOrderRequest {
    dex_api::PlaceOrderRequest { market: None, terms }
}

fn open(terms: OrderTerms) -> ledger_api::OpenEscrowRequest {
    ledger_api::OpenEscrowRequest { market: demo_market(), terms }
}

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

    let escrow: ActionHash = env.call(ALICE, "open_escrow", open(sell_100_at_1_20())).await;
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
    env.assert_supply().await;
}

/// §22: two takers race for the same 100 A. One fills, the other is refunded.
#[tokio::test(flavor = "multi_thread")]
async fn two_takers_cannot_both_fill_the_same_escrow() {
    let env = TestEnv::new(3).await;

    let _: ActionHash = env.call(ALICE, "mint", Amounts::new(10_000, 0)).await;
    let _: ActionHash = env.call(BOB, "mint", Amounts::new(0, 12_000)).await;
    let _: ActionHash = env.call(CAROL, "mint", Amounts::new(0, 12_000)).await;
    let escrow: ActionHash = env.call(ALICE, "open_escrow", open(sell_100_at_1_20())).await;
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
    let winners = [bob.clone(), carol.clone()]
        .iter()
        .filter(|b| **b == Amounts::new(10_000, 0))
        .count();
    let refunded = [bob, carol]
        .iter()
        .filter(|b| **b == Amounts::new(0, 12_000))
        .count();
    assert_eq!((winners, refunded), (1, 1), "one fill, one full refund");
    assert_eq!(env.balance(ALICE).await.available, Amounts::new(0, 12_000));
    env.assert_supply().await;
}

/// Validation rejects debits beyond the author's balance.
#[tokio::test(flavor = "multi_thread")]
async fn cannot_escrow_more_than_the_balance() {
    let env = TestEnv::new(1).await;
    let _: ActionHash = env.call(ALICE, "mint", Amounts::new(5_000, 0)).await;
    let result: ConductorApiResult<ActionHash> = env
        .call_fallible(ALICE, "open_escrow", open(sell_100_at_1_20()))
        .await;
    assert!(result.is_err(), "100 A escrow with only 50 A must be rejected");
    env.assert_supply().await;
}

/// Only the maker may execute their escrow's settlement runs.
#[tokio::test(flavor = "multi_thread")]
async fn only_the_maker_can_run_the_escrow() {
    let env = TestEnv::new(2).await;
    let _: ActionHash = env.call(ALICE, "mint", Amounts::new(10_000, 0)).await;
    let escrow: ActionHash = env.call(ALICE, "open_escrow", open(sell_100_at_1_20())).await;
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
    env.assert_supply().await;
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
    let escrow: ActionHash = env.dex(ALICE, "place_order", place(sell_100_at_1_20())).await;
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
    env.assert_supply().await;
}

/// Validation recomputes the listing tag from the escrow's terms.
#[tokio::test(flavor = "multi_thread")]
async fn listing_with_a_tag_that_disagrees_with_the_escrow_is_rejected() {
    let env = TestEnv::new(2).await;
    let _: ActionHash = env.call(ALICE, "mint", Amounts::new(10_000, 0)).await;
    // An escrow opened through the ledger directly is not listed yet.
    let escrow: ActionHash = env.call(ALICE, "open_escrow", open(sell_100_at_1_20())).await;
    let terms = env.call::<_, EscrowState>(ALICE, "get_escrow_state", escrow.clone()).await.terms;

    let mut cheaper = terms;
    cheaper.price_per_lot = 110;
    let mut other_side = terms;
    other_side.side = Side::Buy;
    let mut later = terms;
    later.expires_at += 1;
    let good = dex_core::listing::encode_tag(&demo_market(), &terms);
    let bad_tags = [
        ("price", dex_core::listing::encode_tag(&demo_market(), &cheaper)),
        ("side", dex_core::listing::encode_tag(&demo_market(), &other_side)),
        ("expiry", dex_core::listing::encode_tag(&demo_market(), &later)),
        ("truncated", good[..16].to_vec()),
    ];
    for (what, tag) in bad_tags {
        let result: ConductorApiResult<ActionHash> = env
            .dex_fallible(ALICE, "list_escrow_raw", RawListing { escrow: escrow.clone(), tag, anchor_market: None })
            .await;
        assert!(result.is_err(), "a listing with the wrong {what} must be rejected");
    }

    // Control: the escrow's own tag passes, so the rejections above are about
    // the tag and the cross-zome escrow decode works.
    let _: ActionHash = env
        .dex(ALICE, "list_escrow_raw", RawListing { escrow: escrow.clone(), tag: good, anchor_market: None })
        .await;
    env.sync().await;
    assert_eq!(env.book(BOB).await.asks, vec![level(120, 100, 1)]);
    env.assert_supply().await;
}

/// Only an escrow's maker may list it.
#[tokio::test(flavor = "multi_thread")]
async fn only_the_maker_can_list_an_escrow() {
    let env = TestEnv::new(2).await;
    let _: ActionHash = env.call(ALICE, "mint", Amounts::new(10_000, 0)).await;
    let escrow: ActionHash = env.call(ALICE, "open_escrow", open(sell_100_at_1_20())).await;
    env.sync().await;

    let result: ConductorApiResult<ActionHash> =
        env.dex_fallible(BOB, "republish_listing", escrow.clone()).await;
    assert!(result.is_err(), "Bob cannot list Alice's escrow");
    assert!(env.book(BOB).await.asks.is_empty());

    let _: ActionHash = env.dex(ALICE, "republish_listing", escrow).await;
    env.sync().await;
    assert_eq!(env.book(BOB).await.asks, vec![level(120, 100, 1)]);
    env.assert_supply().await;
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

    let alice_order: ActionHash = env.dex(ALICE, "place_order", place(sell(40, 120))).await;
    let carol_order: ActionHash = env.dex(CAROL, "place_order", place(sell(35, 121))).await;
    env.sync().await;
    assert_eq!(env.book(BOB).await.asks, vec![level(120, 40, 1), level(121, 35, 1)]);

    let request = TakeRequest {
        market: None,
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
    env.assert_supply().await;
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

    let lasting: ActionHash = env.dex(ALICE, "place_order", place(sell(10, 120))).await;
    let expires_at = Timestamp::now().as_micros() + 30_000_000;
    let short: ActionHash = env
        .dex(ALICE, "place_order", place(OrderTerms { expires_at, ..sell(10, 130) }))
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
        .dex(BOB, "take", TakeRequest { market: None, take: Side::Buy, lots: 10, limit_price: Some(120) })
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
    env.assert_supply().await;
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
        ids.push(env.dex::<_, ActionHash>(maker, "place_order", place(sell(10, 120))).await);
        env.sync().await;
    }
    assert_eq!(env.book(ALICE).await.asks, vec![level(120, 30, 3)]);
    let at_level: Vec<ActionHash> = env
        .dex::<_, Vec<dex_api::Order>>(
            ALICE,
            "get_level_orders",
            dex_api::LevelQuery { market: None, side: Side::Sell, price_per_lot: 120 },
        )
        .await
        .into_iter()
        .map(|o| o.id)
        .collect();
    assert_eq!(at_level, ids, "oldest first");

    let taken: TakeResult = env
        .dex(CAROL, "take", TakeRequest { market: None, take: Side::Buy, lots: 15, limit_price: None })
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
    env.assert_supply().await;
}

/// 21 parks exceed one run's cap of 20. Both `run_my_orders` (fill) and
/// `cancel_order` (release) settle them with two runs in one call.
#[tokio::test(flavor = "multi_thread")]
async fn more_parks_than_one_run_takes_settle_in_one_call() {
    let env = TestEnv::new(2).await;
    let _: ActionHash = env.call(ALICE, "mint", Amounts::new(20_000, 0)).await;
    let _: ActionHash = env.call(BOB, "mint", Amounts::new(0, 60_000)).await;
    let minted = Amounts::new(20_000, 60_000);
    let to_fill: ActionHash = env.dex(ALICE, "place_order", place(sell(100, 120))).await;
    let to_cancel: ActionHash = env.dex(ALICE, "place_order", place(sell(100, 125))).await;
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
    env.assert_supply().await;
}

// ---------------------------------------------------------------------------
// Signals
// ---------------------------------------------------------------------------

type Signals = tokio::sync::broadcast::Receiver<holochain::prelude::Signal>;

impl TestEnv {
    fn signals(&self, agent: usize) -> Signals {
        self.conductors[self.agents[agent].conductor].subscribe_to_app_signals("dex".to_string())
    }

    fn key(&self, agent: usize) -> AgentPubKey {
        self.agents[agent].cell.agent_pubkey().clone()
    }
}

/// The next dex signal on this stream, skipping anything else.
async fn next_dex_signal(signals: &mut Signals) -> dex_api::DexSignal {
    let wait = async {
        loop {
            match signals.recv().await {
                Ok(holochain::prelude::Signal::App { signal, .. }) => {
                    if let Ok(dex) = signal.into_inner().decode::<dex_api::DexSignal>() {
                        return dex;
                    }
                }
                Ok(_) => {}
                Err(e) => panic!("signal stream closed: {e}"),
            }
        }
    };
    tokio::time::timeout(std::time::Duration::from_secs(60), wait)
        .await
        .expect("no dex signal within 60 s")
}

/// A take signals the maker; the maker's client reacts by calling
/// `run_my_orders` (no manual `run_escrow`), which signals the taker to collect.
#[tokio::test(flavor = "multi_thread")]
async fn a_park_signals_the_maker_and_the_run_signals_the_taker() {
    use dex_api::DexSignal;

    let env = TestEnv::new(2).await;
    let mut alice_signals = env.signals(ALICE);
    let mut bob_signals = env.signals(BOB);
    let _: ActionHash = env.call(ALICE, "mint", Amounts::new(4_000, 0)).await;
    let _: ActionHash = env.call(BOB, "mint", Amounts::new(0, 4_800)).await;
    let order: ActionHash = env.dex(ALICE, "place_order", place(sell(40, 120))).await;
    env.sync().await;

    let taken: TakeResult = env
        .dex(BOB, "take", TakeRequest { market: None, take: Side::Buy, lots: 40, limit_price: None })
        .await;
    let park = taken.parks[0].park.clone();

    // Alice's client: a ParkPlaced signal arrives.
    let signal = next_dex_signal(&mut alice_signals).await;
    assert_eq!(
        signal,
        DexSignal::ParkPlaced { escrow: order.clone(), park, taker: env.key(BOB), lots: 40 }
    );
    // It reacts the only way a client should: re-read and run, idempotently.
    // The park may not have reached Alice's view of the DHT yet.
    env.sync().await;
    let reports: Vec<RunReport> = env.dex(ALICE, "run_my_orders", ()).await;
    assert_eq!(reports.iter().map(|r| r.filled_lots).collect::<Vec<_>>(), vec![40]);

    // Bob's client: a RunSettled signal arrives; it collects.
    let signal = next_dex_signal(&mut bob_signals).await;
    assert_eq!(
        signal,
        DexSignal::RunSettled {
            escrow: order,
            run: reports[0].run.clone(),
            maker: env.key(ALICE),
            mode: RunMode::Fill,
        }
    );
    env.sync().await;
    env.collect(BOB).await;
    assert_eq!(env.balance(BOB).await.available, Amounts::new(4_000, 0));
    env.assert_supply().await;
}

// ---------------------------------------------------------------------------
// Market orders
// ---------------------------------------------------------------------------

use dex_api::{MarketBudgetRequest, MarketOrderRequest, MarketResult, MarketRetry, RetryMarketRequest};

fn market(side: Side, lots: u64) -> MarketOrderRequest {
    MarketOrderRequest {
        market: None,
        side,
        lots,
        max_slippage_bps: Some(200),
        expected: None,
    }
}

fn buy(lots: u64, price_per_lot: u64) -> OrderTerms {
    OrderTerms {
        side: Side::Buy,
        ..sell(lots, price_per_lot)
    }
}

async fn run_makers(env: &TestEnv, makers: &[usize]) {
    env.sync().await;
    for &maker in makers {
        let _: Vec<RunReport> = env.dex(maker, "run_my_orders", ()).await;
    }
    env.sync().await;
    collect_all(env).await;
}

/// A market buy sweeps two levels from two makers; each settles at their own
/// price and the average is the exact volume-weighted price.
#[tokio::test(flavor = "multi_thread")]
async fn market_buy_sweeps_two_levels_from_two_makers() {
    let env = TestEnv::new(3).await;
    let _: ActionHash = env.call(ALICE, "mint", Amounts::new(4_000, 0)).await;
    let _: ActionHash = env.call(CAROL, "mint", Amounts::new(3_500, 0)).await;
    let _: ActionHash = env.call(BOB, "mint", Amounts::new(0, 20_000)).await;
    let minted = Amounts::new(7_500, 20_000);
    let alice_order: ActionHash = env.dex(ALICE, "place_order", place(sell(40, 120))).await;
    let carol_order: ActionHash = env.dex(CAROL, "place_order", place(sell(35, 121))).await;
    env.sync().await;

    let result: MarketResult = env.dex(BOB, "market_order", market(Side::Buy, 60)).await;
    let m = &result.plan;
    assert_eq!((m.reference_price, m.limit_price), (Some(120), 123));
    let planned: Vec<(ActionHash, u64)> = result.parks.iter().map(|p| (p.escrow.clone(), p.lots)).collect();
    assert_eq!(planned, vec![(alice_order, 40), (carol_order, 20)]);
    assert_eq!((m.total_quote_minor, m.total_lots), (4_800 + 2_420, 60), "average 72.20 / 60");
    assert_eq!(m.worst_price, Some(121));
    assert_eq!(m.plan.shortfall, 0);

    run_makers(&env, &[ALICE, CAROL]).await;
    assert_eq!(env.balance(BOB).await.available, Amounts::new(6_000, 20_000 - 7_220));
    assert_eq!(env.balance(ALICE).await.available, Amounts::new(0, 4_800));
    assert_eq!(env.balance(CAROL).await.available, Amounts::new(0, 2_420));
    let fills: Vec<Option<u64>> = env
        .dex::<_, Vec<ParkStatus>>(BOB, "my_parks", ())
        .await
        .iter()
        .map(|p| p.settlement.as_ref().map(|s| s.filled_lots))
        .collect();
    assert_eq!(fills, vec![Some(20), Some(40)], "both makers filled in full");
    assert_eq!(env.total_supply().await, minted);
    env.assert_supply().await;
}

/// The slippage limit keeps a far-away level out of the sweep; a budget
/// order reports what it could not spend.
#[tokio::test(flavor = "multi_thread")]
async fn slippage_limit_leaves_a_far_level_untouched() {
    let env = TestEnv::new(3).await;
    let _: ActionHash = env.call(ALICE, "mint", Amounts::new(1_000, 0)).await;
    let _: ActionHash = env.call(CAROL, "mint", Amounts::new(5_000, 0)).await;
    let _: ActionHash = env.call(BOB, "mint", Amounts::new(0, 10_000)).await;
    let minted = Amounts::new(6_000, 10_000);
    let near: ActionHash = env.dex(ALICE, "place_order", place(sell(10, 100))).await;
    let far: ActionHash = env.dex(CAROL, "place_order", place(sell(50, 110))).await;
    env.sync().await;

    // Lots: 30 wanted, only the 10 at 1.00 are within 2% (limit 1.02).
    let preview: dex_api::MarketPlan<ActionHash> = env
        .dex(
            BOB,
            "preview_market_order",
            dex_api::MarketPreviewRequest {
                market: None,
                side: Side::Buy,
                amount: dex_api::MarketAmount::Lots(30),
                max_slippage_bps: None,
            },
        )
        .await;
    assert_eq!((preview.limit_price, preview.plan.filled, preview.plan.shortfall), (102, 10, 20));
    assert_eq!(preview.max_slippage_bps, Some(200), "the default applies");

    // Budget: 30.00 B buys the 10 lots at 1.00 and leaves 20.00 B unspent.
    let result: MarketResult = env
        .dex(
            BOB,
            "market_order_by_budget",
            MarketBudgetRequest {
                market: None,
                side: Side::Buy,
                budget: 3_000,
                max_slippage_bps: Some(200),
                expected: Some(preview),
            },
        )
        .await;
    assert_eq!(result.parks.len(), 1);
    assert_eq!(result.parks[0].escrow, near);
    assert_eq!(result.plan.unspent_budget, Some(2_000));
    assert!(result.changes.is_empty(), "same orders and lots as the preview");

    run_makers(&env, &[ALICE, CAROL]).await;
    let far_state: EscrowState = env.call(BOB, "get_escrow_state", far.clone()).await;
    assert_eq!((far_state.remaining_lots, far_state.runs), (50, 0), "the far level is untouched");
    let far_pending: Vec<ledger_api::PendingPark> = env.call(CAROL, "get_pending_parks", far).await;
    assert!(far_pending.is_empty());
    assert_eq!(env.balance(BOB).await.available, Amounts::new(1_000, 9_000));
    assert_eq!(env.total_supply().await, minted);
    env.assert_supply().await;
}

/// Another taker empties the best level first. The market order's park there
/// is refunded in full; the one retry fills the rest within the ORIGINAL
/// limit, so a level only a fresh limit would reach stays untouched.
#[tokio::test(flavor = "multi_thread")]
async fn a_raced_market_order_is_refunded_and_retried_within_its_original_limit() {
    const DAVE: usize = 3;
    let env = TestEnv::new(4).await;
    let _: ActionHash = env.call(ALICE, "mint", Amounts::new(1_000, 0)).await;
    let _: ActionHash = env.call(CAROL, "mint", Amounts::new(2_500, 0)).await;
    let _: ActionHash = env.call(BOB, "mint", Amounts::new(0, 10_000)).await;
    let _: ActionHash = env.call(DAVE, "mint", Amounts::new(0, 5_000)).await;
    let minted = Amounts::new(3_500, 15_000);
    let best: ActionHash = env.dex(ALICE, "place_order", place(sell(10, 120))).await;
    let next: ActionHash = env.dex(CAROL, "place_order", place(sell(15, 121))).await;
    let beyond: ActionHash = env.dex(CAROL, "place_order", place(sell(10, 124))).await;
    env.sync().await;

    // Dave takes the whole best level first.
    let _: TakeResult = env
        .dex(DAVE, "take", TakeRequest { market: None, take: Side::Buy, lots: 10, limit_price: Some(120) })
        .await;
    env.sync().await;
    // Bob's market buy still sees it (parks do not change remaining lots).
    let original: MarketResult = env.dex(BOB, "market_order", market(Side::Buy, 20)).await;
    assert_eq!(original.plan.limit_price, 123, "ceil(1.20 × 1.02)");
    let planned: Vec<(ActionHash, u64)> = original.parks.iter().map(|p| (p.escrow.clone(), p.lots)).collect();
    assert_eq!(planned, vec![(best.clone(), 10), (next.clone(), 10)]);

    // Alice fills Dave first (time priority) and refunds Bob; Carol fills Bob.
    run_makers(&env, &[ALICE, CAROL]).await;
    let parks: Vec<ParkStatus> = env.dex(BOB, "my_parks", ()).await;
    let at_best = parks.iter().find(|p| p.escrow == best).unwrap();
    assert_eq!(at_best.settlement.as_ref().map(|s| s.filled_lots), Some(0), "refunded in full");
    assert_eq!(env.balance(BOB).await.available, Amounts::new(1_000, 10_000 - 1_210));

    // Retry: 10 unfilled. The new best is 1.21, whose fresh 2% limit (1.24)
    // would reach the 1.24 order; the original 1.23 limit does not.
    let retry: MarketRetry = env
        .dex(BOB, "retry_market_shortfall", RetryMarketRequest { original: original.clone() })
        .await;
    assert_eq!((retry.unfilled_lots, retry.still_pending), (10, 0));
    let again = retry.result.expect("a retry plan");
    assert_eq!(again.plan.limit_price, 123, "the original limit");
    assert_eq!(again.parks.iter().map(|p| (p.escrow.clone(), p.lots)).collect::<Vec<_>>(), vec![(next, 5)]);
    assert_eq!(again.plan.plan.shortfall, 5);
    assert_eq!(again.attempt, 1);

    // A retry cannot be retried.
    let twice: ConductorApiResult<MarketRetry> = env
        .dex_fallible(BOB, "retry_market_shortfall", RetryMarketRequest { original: again })
        .await;
    assert!(twice.is_err());

    run_makers(&env, &[CAROL]).await;
    let beyond_state: EscrowState = env.call(BOB, "get_escrow_state", beyond).await;
    assert_eq!((beyond_state.remaining_lots, beyond_state.runs), (10, 0), "never reached");
    assert_eq!(env.balance(BOB).await.available, Amounts::new(1_500, 10_000 - 1_815));
    assert_eq!(env.total_supply().await, minted);
    env.assert_supply().await;
}

/// Symmetry: a market sell sweeps bids from the highest, paying in A.
#[tokio::test(flavor = "multi_thread")]
async fn market_sell_sweeps_bids() {
    let env = TestEnv::new(3).await;
    let _: ActionHash = env.call(ALICE, "mint", Amounts::new(0, 1_200)).await;
    let _: ActionHash = env.call(CAROL, "mint", Amounts::new(0, 1_180)).await;
    let _: ActionHash = env.call(BOB, "mint", Amounts::new(1_500, 0)).await;
    let minted = Amounts::new(1_500, 2_380);
    let high: ActionHash = env.dex(ALICE, "place_order", place(buy(10, 120))).await;
    let low: ActionHash = env.dex(CAROL, "place_order", place(buy(10, 118))).await;
    env.sync().await;

    let result: MarketResult = env.dex(BOB, "market_order", market(Side::Sell, 15)).await;
    assert_eq!(result.plan.limit_price, 117, "floor(1.20 × 0.98)");
    let planned: Vec<(ActionHash, u64)> = result.parks.iter().map(|p| (p.escrow.clone(), p.lots)).collect();
    assert_eq!(planned, vec![(high, 10), (low, 5)]);
    assert_eq!(result.plan.plan.total_cost, Amounts::new(1_500, 0));
    assert_eq!(result.plan.total_quote_minor, 1_200 + 590);

    run_makers(&env, &[ALICE, CAROL]).await;
    assert_eq!(env.balance(BOB).await.available, Amounts::new(0, 1_790));
    assert_eq!(env.balance(ALICE).await.available, Amounts::new(1_000, 0));
    assert_eq!(env.balance(CAROL).await.available, Amounts::new(500, 0));
    assert_eq!(env.total_supply().await, minted);
    env.assert_supply().await;
}

mod protection;
