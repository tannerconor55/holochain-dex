//! Write-cost benchmark (follow-up to milestone 4): orders placed and settled
//! as they arrive, so each run's escrow is recent, the realistic case. Ignored
//! by default; run with
//!
//! ```text
//! DEX_WRITE_BENCH_MAX=1000 nix develop -c cargo test \
//!   --manifest-path tests/sweettest/Cargo.toml writes:: -- --ignored --nocapture
//! ```
//!
//! One maker; for each order: place it, a taker parks, the maker settles it
//! (every fourth order is cancelled instead). At each checkpoint of history
//! size it times, over the next SAMPLE orders, the pieces of a write:
//! - `settle` / `cancel`: the dex extern (dex coordinator + ledger + commit +
//!   validation);
//! - `run`: the ledger's `run_escrow` alone (ledger coordinator + commit +
//!   validation);
//! - `plan`: the ledger's test-only `plan_run` (ledger coordinator only).
//!
//! `run − plan` is the commit and its inline validation; `settle − run` is
//! the dex coordinator's own work (reads, index link, unlisting, signals).

use super::*;
use std::time::{Duration, Instant};

const SAMPLE: usize = 12;

#[derive(Default)]
struct Times {
    place: Vec<Duration>,
    plan: Vec<Duration>,
    run: Vec<Duration>,
    settle: Vec<Duration>,
    cancel: Vec<Duration>,
}

fn mean(xs: &[Duration]) -> u128 {
    if xs.is_empty() {
        return 0;
    }
    xs.iter().map(|d| d.as_millis()).sum::<u128>() / xs.len() as u128
}

async fn timed<T>(f: impl std::future::Future<Output = T>) -> (T, Duration) {
    let t = Instant::now();
    let v = f.await;
    (v, t.elapsed())
}

/// One order through its life; `measure` records the timings.
async fn one_order(env: &TestEnv, i: usize, measure: Option<&mut Times>) {
    let cancel = i % 4 == 3;
    let (order, place_t) = timed(env.dex::<_, ActionHash>(ALICE, "place_order", place(sell(1, 120)))).await;
    if cancel {
        let (_, cancel_t): (Vec<RunReport>, _) = timed(env.dex(ALICE, "cancel_order", order)).await;
        if let Some(t) = measure {
            t.place.push(place_t);
            t.cancel.push(cancel_t);
        }
        return;
    }
    // The taker's park validates against the escrow: let it reach them.
    env.sync().await;
    let _: ActionHash = env.call(BOB, "park", park_request(&order, 120, 1)).await;
    // The park must reach the maker's view before a run can take it.
    env.sync().await;
    match measure {
        None => {
            let _: Vec<RunReport> = env.dex(ALICE, "settle_order", order).await;
        }
        Some(t) => {
            t.place.push(place_t);
            let input = || RunEscrowInput { escrow: order.clone(), mode: RunMode::Fill };
            let (planned, plan_t): (Option<u64>, _) = timed(env.call(ALICE, "plan_run", input())).await;
            assert_eq!(planned, Some(1), "the park is pending");
            t.plan.push(plan_t);
            // Alternate which path settles, so both are timed at this history.
            if t.run.len() <= t.settle.len() {
                let (_, run_t): (Option<RunReport>, _) = timed(env.call(ALICE, "run_escrow", input())).await;
                t.run.push(run_t);
            } else {
                let (_, settle_t): (Vec<RunReport>, _) = timed(env.dex(ALICE, "settle_order", order.clone())).await;
                t.settle.push(settle_t);
            }
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "benchmark: run explicitly (see module docs)"]
async fn write_cost_by_history_when_orders_settle_as_they_arrive() {
    let max: usize = std::env::var("DEX_WRITE_BENCH_MAX").ok().and_then(|s| s.parse().ok()).unwrap_or(1000);
    let checkpoints: Vec<usize> = [100, 250, 500, 1000].into_iter().filter(|c| *c <= max).collect();
    let env = TestEnv::new(2).await;
    let _: ActionHash = env.call(ALICE, "mint", Amounts::new(100 * (max + checkpoints.len() * SAMPLE) as u64, 0)).await;
    let _: ActionHash = env.call(BOB, "mint", Amounts::new(0, 120 * (max + checkpoints.len() * SAMPLE) as u64)).await;
    env.sync().await;

    let started = Instant::now();
    let mut done = 0;
    let mut rows = Vec::new();
    for &at in &checkpoints {
        while done < at {
            one_order(&env, done, None).await;
            done += 1;
        }
        let mut t = Times::default();
        for k in 0..SAMPLE {
            one_order(&env, done + k, Some(&mut t)).await;
        }
        done += SAMPLE;
        eprintln!(
            "writes: history {at}: place {} ms, plan {} ms, run {} ms, settle {} ms, cancel {} ms ({:.0} s so far)",
            mean(&t.place),
            mean(&t.plan),
            mean(&t.run),
            mean(&t.settle),
            mean(&t.cancel),
            started.elapsed().as_secs_f64()
        );
        rows.push((at, t));
    }
    println!("\n| Orders of history | Place | Ledger coordinator (plan) | Ledger run (+ commit, validation) | Settle (dex) | Cancel (dex) |");
    println!("|---:|---:|---:|---:|---:|---:|");
    for (at, t) in &rows {
        println!(
            "| {at} | {} ms | {} ms | {} ms | {} ms | {} ms |",
            mean(&t.place),
            mean(&t.plan),
            mean(&t.run),
            mean(&t.settle),
            mean(&t.cancel)
        );
    }
    println!("Built {done} orders in {:.0} s.", started.elapsed().as_secs_f64());
    env.assert_supply().await;
}
