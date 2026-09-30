//! Every dex_core unit-test scenario, through the template and dex_core.
//! Each runs as a later run (lock carried in previous_execution) and, where
//! the lock is the full initial lock, also as the opening run (maker spend).

use crate::engine::*;
use dex_core::{Amounts, MarketDef, OrderTerms, RunMode, Side, MAX_PARKS_PER_RUN};

const NOW: i64 = 1_000_000;
const LATER: i64 = 2_000_000;

fn alice_sell_100() -> OrderTerms {
    OrderTerms { side: Side::Sell, price_per_lot: 120, lots: 100, expires_at: LATER }
}

fn case(terms: OrderTerms, lock: Amounts, parks: Vec<Park>, mode: RunMode) -> Case {
    Case { terms, start: Start::Locked(lock), parks, now: NOW, mode }
}

/// Parity as a later run and, when the lock is the whole initial lock, as the
/// opening run too. Returns the later run's result.
fn both(c: Case) -> RhaiRun {
    let lock = prev_locked(&c);
    if Ok(lock) == c.terms.initial_lock(&MarketDef::default_pair()) {
        assert_parity(&Case { start: Start::Opening, ..c.clone() }).expect("opening run succeeds");
    }
    assert_parity(&c).expect("run succeeds")
}

fn paid_to(r: &RhaiRun, who: u8) -> Amounts {
    r.paid.get(&agent_str(who)).cloned().unwrap_or_default()
}

#[test]
fn mvp_success_scenario_chained_through_the_engine() {
    // §30: open, Bob takes 40, Alice cancels the rest. Each run feeds the
    // engine's own output back as previous_execution.
    let terms = alice_sell_100();
    let opening = Case {
        terms,
        start: Start::Opening,
        parks: vec![park(1, BOB, Amounts::new(0, 4_800), 40, NOW - 10)],
        now: NOW,
        mode: RunMode::Fill,
    };
    let fill = assert_parity(&opening).unwrap();
    assert_eq!(paid_to(&fill, BOB), Amounts::new(4_000, 0), "Bob receives 40 A");
    assert_eq!(paid_to(&fill, ALICE), Amounts::new(0, 4_800), "Alice receives 48 B");
    assert_eq!(fill.locked, Amounts::new(6_000, 0), "60 A stays locked");

    let cancel = Case {
        terms,
        start: Start::Chained(fill.engine.output.clone()),
        parks: vec![],
        now: NOW + 1,
        mode: RunMode::Release,
    };
    let release = assert_parity(&cancel).unwrap();
    assert_eq!(paid_to(&release, ALICE), Amounts::new(6_000, 0));
    assert_eq!(release.locked, Amounts::ZERO);
}

#[test]
fn buy_order_is_symmetric() {
    let terms = OrderTerms { side: Side::Buy, ..alice_sell_100() };
    let lock = terms.initial_lock(&MarketDef::default_pair()).unwrap();
    let r = both(case(terms, lock, vec![park(1, CAROL, Amounts::new(4_000, 0), 40, NOW)], RunMode::Fill));
    assert_eq!(paid_to(&r, CAROL), Amounts::new(0, 4_800), "seller gets 48 B");
    assert_eq!(paid_to(&r, ALICE), Amounts::new(4_000, 0), "buyer gets 40 A");
    assert_eq!(r.locked, Amounts::new(0, 7_200));
}

#[test]
fn two_takers_cannot_both_consume_the_same_escrow() {
    let terms = alice_sell_100();
    let r = both(case(
        terms,
        terms.initial_lock(&MarketDef::default_pair()).unwrap(),
        vec![
            park(2, CAROL, Amounts::new(0, 12_000), 100, NOW - 5),
            park(1, BOB, Amounts::new(0, 12_000), 100, NOW - 9),
        ],
        RunMode::Fill,
    ));
    assert_eq!(paid_to(&r, BOB), Amounts::new(10_000, 0), "Bob fills all 100 A");
    assert_eq!(paid_to(&r, CAROL), Amounts::new(0, 12_000), "Carol fully refunded");
    assert_eq!(r.locked, Amounts::ZERO);
    assert_eq!(r.consumed, vec![park_id(1), park_id(2)], "time order");
}

#[test]
fn time_priority_partially_fills_the_later_taker() {
    let terms = alice_sell_100();
    let r = both(case(
        terms,
        terms.initial_lock(&MarketDef::default_pair()).unwrap(),
        vec![
            park(1, BOB, Amounts::new(0, 8_400), 70, NOW - 9),
            park(2, CAROL, Amounts::new(0, 6_000), 50, NOW - 5),
        ],
        RunMode::Fill,
    ));
    assert_eq!(paid_to(&r, BOB), Amounts::new(7_000, 0));
    assert_eq!(paid_to(&r, CAROL), Amounts::new(3_000, 6_000 - 3_600));
    assert_eq!(paid_to(&r, ALICE), Amounts::new(0, 8_400 + 3_600));
    assert_eq!(r.filled_lots, 100);
}

#[test]
fn equal_timestamps_break_ties_by_source_hash() {
    let r = both(case(
        alice_sell_100(),
        Amounts::new(1_000, 0),
        vec![
            park(9, CAROL, Amounts::new(0, 1_200), 10, NOW),
            park(3, BOB, Amounts::new(0, 1_200), 10, NOW),
        ],
        RunMode::Fill,
    ));
    // park_id(3) sorts before park_id(9) as a string, as 3 < 9 in dex_core.
    assert!(park_id(3) < park_id(9));
    assert_eq!(paid_to(&r, BOB), Amounts::new(1_000, 0));
    assert_eq!(paid_to(&r, CAROL), Amounts::new(0, 1_200));
}

#[test]
fn overpayment_is_refunded() {
    let terms = alice_sell_100();
    let r = both(case(terms, terms.initial_lock(&MarketDef::default_pair()).unwrap(), vec![park(1, BOB, Amounts::new(0, 5_000), 40, NOW)], RunMode::Fill));
    assert_eq!(paid_to(&r, BOB), Amounts::new(4_000, 200), "40 A plus 2.00 B change");
    assert_eq!(paid_to(&r, ALICE), Amounts::new(0, 4_800));
}

#[test]
fn underpayment_fills_only_what_is_covered() {
    let terms = alice_sell_100();
    let r = both(case(terms, terms.initial_lock(&MarketDef::default_pair()).unwrap(), vec![park(1, BOB, Amounts::new(0, 4_000), 40, NOW)], RunMode::Fill));
    assert_eq!(r.outcomes[0].1, 33);
    assert_eq!(paid_to(&r, BOB), Amounts::new(3_300, 40));
    assert_eq!(r.locked, Amounts::new(6_700, 0));
}

#[test]
fn wrong_asset_is_refunded_in_full() {
    let terms = alice_sell_100();
    let r = both(case(terms, terms.initial_lock(&MarketDef::default_pair()).unwrap(), vec![park(1, BOB, Amounts::new(500, 0), 5, NOW)], RunMode::Fill));
    assert_eq!(paid_to(&r, BOB), Amounts::new(500, 0));
    assert_eq!(paid_to(&r, ALICE), Amounts::ZERO);
    assert_eq!(r.locked, terms.initial_lock(&MarketDef::default_pair()).unwrap());
}

#[test]
fn expired_order_refunds_takers_and_keeps_lock_until_release() {
    let terms = OrderTerms { expires_at: NOW, ..alice_sell_100() };
    let lock = terms.initial_lock(&MarketDef::default_pair()).unwrap();
    let r = both(case(terms, lock.clone(), vec![park(1, BOB, Amounts::new(0, 4_800), 40, NOW - 1)], RunMode::Fill));
    assert_eq!(paid_to(&r, BOB), Amounts::new(0, 4_800), "no fill at or after expiry");
    assert_eq!(r.locked, lock);
    let release = both(case(terms, lock.clone(), vec![], RunMode::Release));
    assert_eq!(paid_to(&release, ALICE), lock);
}

#[test]
fn release_refunds_pending_takers_too() {
    let r = both(case(alice_sell_100(), Amounts::new(6_000, 0), vec![park(1, BOB, Amounts::new(0, 4_800), 40, NOW)], RunMode::Release));
    assert_eq!(paid_to(&r, BOB), Amounts::new(0, 4_800));
    assert_eq!(paid_to(&r, ALICE), Amounts::new(6_000, 0));
    assert_eq!(r.locked, Amounts::ZERO);
}

#[test]
fn runs_after_close_only_refund() {
    let r = both(case(alice_sell_100(), Amounts::ZERO, vec![park(1, BOB, Amounts::new(0, 4_800), 40, NOW)], RunMode::Fill));
    assert_eq!(paid_to(&r, BOB), Amounts::new(0, 4_800));
    assert_eq!(r.filled_lots, 0);
}

#[test]
fn self_trade_nets_into_one_allocation() {
    let terms = alice_sell_100();
    let r = both(case(terms, terms.initial_lock(&MarketDef::default_pair()).unwrap(), vec![park(1, ALICE, Amounts::new(0, 1_200), 10, NOW)], RunMode::Fill));
    assert_eq!(r.paid.len(), 1);
    assert_eq!(paid_to(&r, ALICE), Amounts::new(1_000, 1_200));
}

#[test]
fn input_order_does_not_change_the_result() {
    let terms = alice_sell_100();
    let parks = vec![
        park(1, BOB, Amounts::new(0, 6_000), 50, NOW - 3),
        park(2, CAROL, Amounts::new(0, 9_600), 80, NOW - 2),
        park(3, BOB, Amounts::new(0, 1_200), 10, NOW - 1),
    ];
    let mut reversed = parks.clone();
    reversed.reverse();
    let lock = terms.initial_lock(&MarketDef::default_pair()).unwrap();
    let forward = both(case(terms, lock.clone(), parks, RunMode::Fill));
    let backward = both(case(terms, lock, reversed, RunMode::Fill));
    assert_eq!(forward.paid, backward.paid);
    assert_eq!(forward.consumed, backward.consumed);
    assert_eq!(forward.locked, backward.locked);
}

#[test]
fn parks_beyond_the_cap_wait_for_the_next_run() {
    let terms = alice_sell_100();
    let parks: Vec<Park> = (0..=MAX_PARKS_PER_RUN as u32)
        .map(|i| park(i, BOB, Amounts::new(0, 120), 1, NOW - 100 + i64::from(i)))
        .collect();
    let first = both(case(terms, terms.initial_lock(&MarketDef::default_pair()).unwrap(), parks.clone(), RunMode::Fill));
    assert_eq!(first.consumed.len(), MAX_PARKS_PER_RUN);
    assert_eq!(first.rejected, vec![park_id(MAX_PARKS_PER_RUN as u32)], "the newest waits");

    // The next run picks the deferred park up.
    let next = Case {
        terms,
        start: Start::Chained(first.engine.output.clone()),
        parks: vec![parks[MAX_PARKS_PER_RUN].clone()],
        now: NOW + 1,
        mode: RunMode::Fill,
    };
    let second = assert_parity(&next).unwrap();
    assert_eq!(second.filled_lots, 1);
    assert_eq!(second.locked, Amounts::new(10_000 - 2_100, 0));
}

#[test]
fn a_park_listed_twice_is_refused_by_both() {
    let terms = alice_sell_100();
    let p = park(1, BOB, Amounts::new(0, 120), 1, NOW);
    let c = case(terms, terms.initial_lock(&MarketDef::default_pair()).unwrap(), vec![p.clone(), p], RunMode::Fill);
    assert!(run_rhai(&c).is_err());
    assert!(assert_parity(&c).is_none(), "both refuse");
}

#[test]
fn a_lock_that_is_not_whole_lots_is_refused_by_both() {
    let c = case(alice_sell_100(), Amounts::new(150, 0), vec![], RunMode::Fill);
    assert!(assert_parity(&c).is_none());
    // A lock holding the taker asset is just as inconsistent.
    let c = case(alice_sell_100(), Amounts::new(1_000, 5), vec![], RunMode::Fill);
    assert!(assert_parity(&c).is_none());
}

#[test]
fn invalid_terms_are_refused_by_both() {
    for terms in [
        OrderTerms { lots: 0, ..alice_sell_100() },
        OrderTerms { price_per_lot: 0, ..alice_sell_100() },
    ] {
        let c = case(terms, Amounts::ZERO, vec![], RunMode::Fill);
        assert!(assert_parity(&c).is_none(), "{terms:?}");
    }
}

// ---------------------------------------------------------------------------
// Template-only rules (no dex_core counterpart)
// ---------------------------------------------------------------------------

#[test]
fn the_opening_run_needs_exactly_the_initial_lock() {
    let terms = alice_sell_100();
    // An opening case whose terms ask for more than the maker spend holds:
    // build the input for 100 lots, then run it against terms for 101.
    let opening = Case { terms, start: Start::Opening, parks: vec![], now: NOW, mode: RunMode::Fill };
    assert!(run_rhai(&opening).is_ok());
    let mut input = input_json(&opening);
    input["inputs"]["lots"]["data"] = serde_json::json!(101);
    let code = rmp_serde::to_vec(TEMPLATE).unwrap();
    let result = rave_engine::prelude::RhaiEngine::new().execute(
        &input,
        code,
        Some(rave_engine::prelude::PresetVariables {
            ea_id: hdi::prelude::ActionHash::from_raw_32(vec![0x22; 32]),
            executor: agent(ALICE),
            executed_timestamp: hdi::prelude::Timestamp::from_micros(NOW),
        }),
    );
    let err = format!("{result:?}");
    assert!(err.contains("the opening spend must equal the initial lock"), "{err}");
}

#[test]
fn the_opening_run_names_the_maker_spend_in_a_name_only_allocation() {
    let terms = alice_sell_100();
    let r = assert_parity(&Case { terms, start: Start::Opening, parks: vec![], now: NOW, mode: RunMode::Fill }).unwrap();
    let allocations = r.engine.output.unyt_allocation.clone().unwrap();
    assert_eq!(allocations.len(), 1);
    assert_eq!(allocations[0].receiver.to_string(), agent_str(ALICE));
    assert!(serde_json::to_value(&allocations[0].amounts).unwrap().as_object().unwrap().is_empty(), "amounts {{}}");
    assert_eq!(r.locked, terms.initial_lock(&MarketDef::default_pair()).unwrap());
}
