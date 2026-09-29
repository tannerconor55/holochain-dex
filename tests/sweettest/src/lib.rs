//! Sweettest harness for the DEX MVP.
//!
//! Multi-conductor tests of the mock-ledger settlement flow, including the
//! end-to-end demonstration in §30 of the protocol spec.
//!
//! Needs the packed DNA: run `./build.sh` first (or set `DEX_DNA_PATH`).

// Test-only crate: without this, the non-test library build that `cargo test`
// also makes (for doctests) reports every fixture as dead code.
#![cfg(test)]

use holochain::prelude::*;
use holochain::conductor::api::error::ConductorApiResult;
use holochain::sweettest::*;
use ledger_api::{
    Amounts, BalanceView, EscrowState, OrderTerms, ParkRequest, RunEscrowInput, RunMode, RunReport,
    Side,
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
        let mut conductors = SweetConductorBatch::from_standard_config_rendezvous(n).await;
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

    async fn call<I, O>(&self, agent: usize, f: &str, input: I) -> O
    where
        I: Serialize + std::fmt::Debug,
        O: serde::de::DeserializeOwned + std::fmt::Debug,
    {
        let a = &self.agents[agent];
        self.conductors[a.conductor]
            .call(&a.cell.zome("ledger"), f, input)
            .await
    }

    async fn call_fallible<I, O>(&self, agent: usize, f: &str, input: I) -> ConductorApiResult<O>
    where
        I: Serialize + std::fmt::Debug,
        O: serde::de::DeserializeOwned + std::fmt::Debug,
    {
        let a = &self.agents[agent];
        self.conductors[a.conductor]
            .call_fallible(&a.cell.zome("ledger"), f, input)
            .await
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
