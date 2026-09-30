//! Operation budget. Rhai does not expose its counter, so the harness finds the
//! smallest `max_operations` a run succeeds under by binary search. The
//! default budget is 100,000 [engine: `RhaiEngineConfig::default`]; the run at
//! the 20-park cap must stay within half of it.

use crate::engine::*;
use dex_core::{Amounts, OrderTerms, RunMode, Side, MAX_PARKS_PER_RUN};
use rave_engine::prelude::RhaiEngineConfig;

const NOW: i64 = 1_000_000;
const DEFAULT_BUDGET: u64 = 100_000;

fn operations(case: &Case) -> u64 {
    let runs = |limit: u64| run_engine(case, RhaiEngineConfig::default().with_max_operations(limit as usize)).is_ok();
    assert!(runs(DEFAULT_BUDGET * 10), "the case runs at all");
    let (mut lo, mut hi) = (1u64, DEFAULT_BUDGET * 10);
    while lo < hi {
        let mid = (lo + hi) / 2;
        if runs(mid) {
            hi = mid;
        } else {
            lo = mid + 1;
        }
    }
    lo
}

fn with_parks(n: u32, start: Start) -> Case {
    let terms = OrderTerms { side: Side::Sell, price_per_lot: 120, lots: 10_000, expires_at: 2_000_000 };
    Case {
        terms,
        start,
        // Partial fills with change, the most work per park.
        parks: (0..n).map(|i| park(i, BOB + (i % 2) as u8, Amounts::new(0, 1_250), 10, NOW - 1_000 + i64::from(i))).collect(),
        now: NOW,
        mode: RunMode::Fill,
    }
}

#[test]
fn the_cap_run_uses_at_most_half_the_default_budget() {
    let lock = Start::Locked(Amounts::new(1_000_000, 0));
    let empty = operations(&with_parks(0, lock.clone()));
    let one = operations(&with_parks(1, lock.clone()));
    let cap = operations(&with_parks(MAX_PARKS_PER_RUN as u32, lock.clone()));
    let over = operations(&with_parks(MAX_PARKS_PER_RUN as u32 + 5, lock.clone()));
    let opening = operations(&with_parks(MAX_PARKS_PER_RUN as u32, Start::Opening));
    let per_park = (cap - empty) / MAX_PARKS_PER_RUN as u64;
    println!(
        "operations: no parks {empty}, 1 park {one}, {MAX_PARKS_PER_RUN} parks {cap} \
         ({per_park} per park), opening with {MAX_PARKS_PER_RUN} parks {opening}, \
         {} parks (5 deferred) {over}; default budget {DEFAULT_BUDGET}",
        MAX_PARKS_PER_RUN + 5
    );
    assert!(cap * 2 <= DEFAULT_BUDGET, "the cap run uses {cap} of {DEFAULT_BUDGET}");
    assert!(opening * 2 <= DEFAULT_BUDGET, "the opening cap run uses {opening}");
}
