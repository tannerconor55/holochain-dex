//! Generated cases, from the same generator as dex_core's
//! `conservation_holds_across_generated_runs` (same seed, same LCG, same
//! draws in the same order), each run as the opening run and as a later run.

use crate::engine::*;
use dex_core::properties::Timing;
use dex_core::timeout::park_deadline;
use dex_core::{Amounts, MarketDef, OrderTerms, RunMode, Side, MAX_PARKS_PER_RUN};

const NOW: i64 = 1_000_000;
const LATER: i64 = 2_000_000;
const CASES: usize = 2_000;

/// Deadlines straddling NOW: parked at 0..9 µs, a park is consumable at NOW
/// only when parked at 5 µs or later (and never once the order expired at
/// NOW), so most cases mix consumable and expired parks. Derived from the
/// drawn values, not drawn, so the sequence matches dex_core's generator.
const TIMING: Timing = Timing { park_timeout_us: 999_995, settle_grace_us: 1 };

fn generated() -> Vec<Case> {
    let mut seed: u64 = 0x5eed;
    let mut next = move |m: u64| {
        seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        (seed >> 33) % m
    };
    let mut cases = Vec::with_capacity(CASES);
    for _ in 0..CASES {
        let terms = OrderTerms {
            side: if next(2) == 0 { Side::Sell } else { Side::Buy },
            price_per_lot: 1 + next(500),
            lots: 1 + next(200),
            expires_at: if next(4) == 0 { NOW } else { LATER },
        };
        let takers = [BOB, CAROL, ALICE];
        let parks = (0..next(MAX_PARKS_PER_RUN as u64 + 1) as u32)
            .map(|i| {
                park(
                    i,
                    takers[next(3) as usize],
                    Amounts::new(next(30_000), next(30_000)),
                    next(120),
                    next(10) as i64,
                )
            })
            .map(|p| {
                let deadline = park_deadline(p.parked_at, terms.expires_at, &TIMING);
                p.until(deadline)
            })
            .collect();
        let mode = if next(3) == 0 { RunMode::Release } else { RunMode::Fill };
        cases.push(Case { terms, start: Start::Opening, parks, now: NOW, mode });
    }
    cases
}

#[test]
fn template_matches_execute_run_on_generated_cases() {
    let mut runs = 0;
    let mut fills = 0;
    let mut expired = 0;
    for c in generated() {
        expired += c.parks.iter().filter(|p| p.deadline.is_some_and(|d| NOW >= d)).count();
        let lock = c.terms.initial_lock(&MarketDef::default_pair()).unwrap();
        for start in [Start::Opening, Start::Locked(lock)] {
            let case = Case { start, ..c.clone() };
            let r = assert_parity(&case).expect("every generated case is valid");
            runs += 1;
            fills += r.filled_lots;
        }
    }
    assert_eq!(runs, 2 * CASES);
    assert!(fills > 0, "the generator fills something");
    assert!(expired > CASES, "the generator puts parks past their deadline: {expired}");
    println!("parity: {runs} runs identical to dex_core ({fills} lots filled, {expired} parks past their deadline)");
}

#[test]
fn template_matches_execute_run_from_partial_locks() {
    // The same cases from a lock with some lots already sold, so remaining <
    // requested is exercised on both sides.
    for c in generated().into_iter().take(500) {
        let half = c.terms.lots / 2;
        let part = match c.terms.side {
            Side::Sell => Amounts::new(half * MarketDef::default_pair().lot_size, 0),
            Side::Buy => Amounts::new(0, half * c.terms.price_per_lot),
        };
        assert_parity(&Case { start: Start::Locked(part), ..c });
    }
}
