use super::*;

const ALICE: &str = "alice";
const BOB: &str = "bob";
const CAROL: &str = "carol";
const NOW: i64 = 1_000_000;
const LATER: i64 = 2_000_000;

/// Alice sells 100 A at 1.20 B per A (§30 of the protocol).
fn alice_sell_100() -> OrderTerms {
    OrderTerms {
        side: Side::Sell,
        price_per_lot: 120,
        lots: 100,
        expires_at: LATER,
    }
}

fn park(id: u32, taker: &'static str, amounts: Amounts, lots: u64, at: i64) -> ParkInput<u32, &'static str> {
    ParkInput {
        id,
        taker,
        amounts,
        requested_lots: lots,
        parked_at: at,
    }
}

fn run(
    terms: OrderTerms,
    prev_locked: Amounts,
    parks: Vec<ParkInput<u32, &'static str>>,
    mode: RunMode,
) -> RunOutput<u32, &'static str> {
    execute_run(&RunInput {
        terms,
        maker: ALICE,
        prev_locked,
        parks,
        now: NOW,
        mode,
    })
    .expect("run should succeed")
}

fn paid_to(out: &RunOutput<u32, &'static str>, who: &str) -> Amounts {
    out.allocations
        .iter()
        .find(|a| a.receiver == who)
        .map(|a| a.amounts.clone())
        .unwrap_or_default()
}

#[test]
fn mvp_success_scenario() {
    let terms = alice_sell_100();
    let lock = terms.initial_lock().unwrap();
    assert_eq!(lock, Amounts::new(10_000, 0), "100.00 A escrowed");

    // Bob parks 48.00 B for 40 A.
    let fill = run(
        terms,
        lock,
        vec![park(1, BOB, Amounts::new(0, 4_800), 40, NOW - 10)],
        RunMode::Fill,
    );
    assert_eq!(paid_to(&fill, BOB), Amounts::new(4_000, 0), "Bob receives 40 A");
    assert_eq!(paid_to(&fill, ALICE), Amounts::new(0, 4_800), "Alice receives 48 B");
    assert_eq!(fill.locked, Amounts::new(6_000, 0), "60 A stays locked");
    assert_eq!(terms.remaining_lots(&fill.locked).unwrap(), 60);

    // Alice cancels: the remaining 60 A comes back.
    let release = run(terms, fill.locked, vec![], RunMode::Release);
    assert_eq!(paid_to(&release, ALICE), Amounts::new(6_000, 0));
    assert_eq!(release.locked, Amounts::ZERO);
}

#[test]
fn buy_order_is_symmetric() {
    // Bob (as maker here, named ALICE in the run) buys 100 A at max 1.20 B.
    let terms = OrderTerms {
        side: Side::Buy,
        ..alice_sell_100()
    };
    let lock = terms.initial_lock().unwrap();
    assert_eq!(lock, Amounts::new(0, 12_000), "120.00 B escrowed");

    let out = run(
        terms,
        lock,
        vec![park(1, CAROL, Amounts::new(4_000, 0), 40, NOW)],
        RunMode::Fill,
    );
    assert_eq!(paid_to(&out, CAROL), Amounts::new(0, 4_800), "seller gets 48 B");
    assert_eq!(paid_to(&out, ALICE), Amounts::new(4_000, 0), "buyer gets 40 A");
    assert_eq!(out.locked, Amounts::new(0, 7_200), "72 B stays locked");
}

#[test]
fn two_takers_cannot_both_consume_the_same_escrow() {
    let terms = alice_sell_100();
    let lock = terms.initial_lock().unwrap();
    // Both want all 100 A; Bob parked first.
    let out = run(
        terms,
        lock,
        vec![
            park(2, CAROL, Amounts::new(0, 12_000), 100, NOW - 5),
            park(1, BOB, Amounts::new(0, 12_000), 100, NOW - 9),
        ],
        RunMode::Fill,
    );
    assert_eq!(paid_to(&out, BOB), Amounts::new(10_000, 0), "Bob fills all 100 A");
    assert_eq!(paid_to(&out, CAROL), Amounts::new(0, 12_000), "Carol fully refunded");
    assert_eq!(out.locked, Amounts::ZERO);
    assert_eq!(out.consumed, vec![1, 2], "processed in time order");
}

#[test]
fn time_priority_partially_fills_the_later_taker() {
    let terms = alice_sell_100();
    let lock = terms.initial_lock().unwrap();
    let out = run(
        terms,
        lock,
        vec![
            park(1, BOB, Amounts::new(0, 8_400), 70, NOW - 9),
            park(2, CAROL, Amounts::new(0, 6_000), 50, NOW - 5),
        ],
        RunMode::Fill,
    );
    // Bob 70 lots, Carol gets the remaining 30 and a refund for the other 20.
    assert_eq!(paid_to(&out, BOB), Amounts::new(7_000, 0));
    assert_eq!(paid_to(&out, CAROL), Amounts::new(3_000, 6_000 - 3_600));
    assert_eq!(paid_to(&out, ALICE), Amounts::new(0, 8_400 + 3_600));
    assert_eq!(out.filled_lots, 100);
}

#[test]
fn equal_timestamps_break_ties_by_id() {
    let terms = alice_sell_100();
    let out = run(
        terms,
        Amounts::new(1_000, 0), // 10 lots left
        vec![
            park(9, CAROL, Amounts::new(0, 1_200), 10, NOW),
            park(3, BOB, Amounts::new(0, 1_200), 10, NOW),
        ],
        RunMode::Fill,
    );
    assert_eq!(paid_to(&out, BOB), Amounts::new(1_000, 0));
    assert_eq!(paid_to(&out, CAROL), Amounts::new(0, 1_200));
}

#[test]
fn overpayment_is_refunded() {
    let terms = alice_sell_100();
    let out = run(
        terms,
        terms.initial_lock().unwrap(),
        vec![park(1, BOB, Amounts::new(0, 5_000), 40, NOW)],
        RunMode::Fill,
    );
    assert_eq!(paid_to(&out, BOB), Amounts::new(4_000, 200), "40 A plus 2.00 B change");
    assert_eq!(paid_to(&out, ALICE), Amounts::new(0, 4_800));
}

#[test]
fn underpayment_fills_only_what_is_covered() {
    let terms = alice_sell_100();
    // 40.00 B covers 33 lots at 1.20 (39.60 B); 0.40 B refunded.
    let out = run(
        terms,
        terms.initial_lock().unwrap(),
        vec![park(1, BOB, Amounts::new(0, 4_000), 40, NOW)],
        RunMode::Fill,
    );
    assert_eq!(out.outcomes[0].filled_lots, 33);
    assert_eq!(paid_to(&out, BOB), Amounts::new(3_300, 40));
    assert_eq!(out.locked, Amounts::new(6_700, 0));
}

#[test]
fn wrong_asset_is_refunded_in_full() {
    let terms = alice_sell_100();
    let out = run(
        terms,
        terms.initial_lock().unwrap(),
        vec![park(1, BOB, Amounts::new(500, 0), 5, NOW)],
        RunMode::Fill,
    );
    assert_eq!(paid_to(&out, BOB), Amounts::new(500, 0));
    assert_eq!(paid_to(&out, ALICE), Amounts::ZERO);
    assert_eq!(out.locked, terms.initial_lock().unwrap());
}

#[test]
fn expired_order_refunds_takers_and_keeps_lock_until_release() {
    let terms = OrderTerms {
        expires_at: NOW,
        ..alice_sell_100()
    };
    let lock = terms.initial_lock().unwrap();
    let out = run(
        terms,
        lock.clone(),
        vec![park(1, BOB, Amounts::new(0, 4_800), 40, NOW - 1)],
        RunMode::Fill,
    );
    assert_eq!(paid_to(&out, BOB), Amounts::new(0, 4_800), "no fill after expiry");
    assert_eq!(out.locked, lock);

    let release = run(terms, lock.clone(), vec![], RunMode::Release);
    assert_eq!(paid_to(&release, ALICE), lock);
}

#[test]
fn release_refunds_pending_takers_too() {
    let terms = alice_sell_100();
    let out = run(
        terms,
        Amounts::new(6_000, 0),
        vec![park(1, BOB, Amounts::new(0, 4_800), 40, NOW)],
        RunMode::Release,
    );
    assert_eq!(paid_to(&out, BOB), Amounts::new(0, 4_800));
    assert_eq!(paid_to(&out, ALICE), Amounts::new(6_000, 0));
    assert_eq!(out.locked, Amounts::ZERO);
}

#[test]
fn runs_after_close_only_refund() {
    let terms = alice_sell_100();
    let out = run(
        terms,
        Amounts::ZERO,
        vec![park(1, BOB, Amounts::new(0, 4_800), 40, NOW)],
        RunMode::Fill,
    );
    assert_eq!(paid_to(&out, BOB), Amounts::new(0, 4_800));
    assert_eq!(out.filled_lots, 0);
}

#[test]
fn self_trade_nets_into_one_allocation() {
    let terms = alice_sell_100();
    let out = run(
        terms,
        terms.initial_lock().unwrap(),
        vec![park(1, ALICE, Amounts::new(0, 1_200), 10, NOW)],
        RunMode::Fill,
    );
    assert_eq!(out.allocations.len(), 1);
    assert_eq!(paid_to(&out, ALICE), Amounts::new(1_000, 1_200));
}

#[test]
fn park_order_does_not_change_the_result() {
    let terms = alice_sell_100();
    let parks = vec![
        park(1, BOB, Amounts::new(0, 6_000), 50, NOW - 3),
        park(2, CAROL, Amounts::new(0, 9_600), 80, NOW - 2),
        park(3, BOB, Amounts::new(0, 1_200), 10, NOW - 1),
    ];
    let mut reversed = parks.clone();
    reversed.reverse();
    let lock = terms.initial_lock().unwrap();
    assert_eq!(
        run(terms, lock.clone(), parks, RunMode::Fill),
        run(terms, lock, reversed, RunMode::Fill)
    );
}

#[test]
fn rejects_more_parks_than_the_cap() {
    let terms = alice_sell_100();
    let parks = (0..=MAX_PARKS_PER_RUN as u32)
        .map(|i| park(i, BOB, Amounts::new(0, 120), 1, NOW))
        .collect();
    let err = execute_run(&RunInput {
        terms,
        maker: ALICE,
        prev_locked: terms.initial_lock().unwrap(),
        parks,
        now: NOW,
        mode: RunMode::Fill,
    })
    .unwrap_err();
    assert_eq!(err, CoreError::TooManyParks);
}

#[test]
fn rejects_duplicate_parks() {
    let terms = alice_sell_100();
    let p = park(1, BOB, Amounts::new(0, 120), 1, NOW);
    let err = execute_run(&RunInput {
        terms,
        maker: ALICE,
        prev_locked: terms.initial_lock().unwrap(),
        parks: vec![p.clone(), p],
        now: NOW,
        mode: RunMode::Fill,
    })
    .unwrap_err();
    assert_eq!(err, CoreError::DuplicatePark);
}

#[test]
fn rejects_a_lock_that_is_not_whole_lots() {
    let terms = alice_sell_100();
    let err = execute_run::<u32, &str>(&RunInput {
        terms,
        maker: ALICE,
        prev_locked: Amounts::new(150, 0),
        parks: vec![],
        now: NOW,
        mode: RunMode::Fill,
    })
    .unwrap_err();
    assert_eq!(err, CoreError::InconsistentLock);
}

#[test]
fn select_parks_takes_the_oldest_up_to_the_cap() {
    let pending: Vec<_> = (0..30u32)
        .rev()
        .map(|i| park(i, BOB, Amounts::new(0, 120), 1, i as i64))
        .collect();
    let chosen = select_parks(pending);
    assert_eq!(chosen.len(), MAX_PARKS_PER_RUN);
    assert_eq!(chosen.first().unwrap().id, 0);
    assert_eq!(chosen.last().unwrap().id, MAX_PARKS_PER_RUN as u32 - 1);
}

/// Conservation over many generated runs, both sides and both modes.
#[test]
fn conservation_holds_across_generated_runs() {
    let mut seed: u64 = 0x5eed;
    let mut next = move |m: u64| {
        seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        (seed >> 33) % m
    };
    for _ in 0..2_000 {
        let terms = OrderTerms {
            side: if next(2) == 0 { Side::Sell } else { Side::Buy },
            price_per_lot: 1 + next(500),
            lots: 1 + next(200),
            expires_at: if next(4) == 0 { NOW } else { LATER },
        };
        let lock = terms.initial_lock().unwrap();
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
            .collect();
        let mode = if next(3) == 0 { RunMode::Release } else { RunMode::Fill };
        let out = run(terms, lock, parks, mode);
        // execute_run already checks conservation; also check the lock stays
        // consistent so the next run can use it.
        assert!(terms.remaining_lots(&out.locked).unwrap() <= terms.lots);
    }
}

// ---------------------------------------------------------------------------
// Amounts: a normalised, ordered unit map
// ---------------------------------------------------------------------------

#[test]
fn amounts_never_store_zero_units() {
    assert_eq!(Amounts::new(0, 0), Amounts::ZERO);
    assert!(Amounts::new(0, 0).is_zero());
    assert_eq!(Amounts::new(5, 0), Amounts::of(UNIT_A, 5));
    // Subtracting to zero removes the unit, so the result equals a fresh map.
    let back = Amounts::new(5, 7).checked_sub(&Amounts::new(5, 0)).unwrap();
    assert_eq!(back, Amounts::of(HUB_UNIT, 7));
    assert_eq!(back.units().collect::<Vec<_>>(), vec![(HUB_UNIT, 7)]);
}

#[test]
fn equal_amounts_serialise_identically_in_unit_order() {
    let built_one_way = Amounts::of("Z", 1).checked_add(&Amounts::of(UNIT_A, 2)).unwrap();
    let built_other_way = Amounts::of(UNIT_A, 2).checked_add(&Amounts::of("Z", 1)).unwrap();
    let a = serde_json::to_string(&built_one_way).unwrap();
    let b = serde_json::to_string(&built_other_way).unwrap();
    assert_eq!(a, b);
    assert_eq!(a, r#"{"A":2,"Z":1}"#, "a plain map, ordered by unit id");
}

#[test]
fn a_zero_valued_or_unnamed_unit_does_not_deserialise() {
    assert!(serde_json::from_str::<Amounts>(r#"{"A":0}"#).is_err());
    assert!(serde_json::from_str::<Amounts>(r#"{"A":1,"HF":0}"#).is_err());
    assert!(serde_json::from_str::<Amounts>(r#"{"":1}"#).is_err());
    assert_eq!(serde_json::from_str::<Amounts>(r#"{"HF":4800}"#).unwrap(), Amounts::new(0, 4_800));
    assert_eq!(serde_json::from_str::<Amounts>("{}").unwrap(), Amounts::ZERO);
}

#[test]
fn amounts_arithmetic_is_checked_per_unit() {
    let max = Amounts::of(UNIT_A, u64::MAX);
    assert_eq!(max.checked_add(&Amounts::of(UNIT_A, 1)), None, "overflow");
    assert!(max.checked_add(&Amounts::of(HUB_UNIT, 1)).is_some(), "other units are independent");
    assert_eq!(Amounts::new(1, 5).checked_sub(&Amounts::new(2, 0)), None, "underflow in any unit");
    assert_eq!(Amounts::new(1, 5).checked_sub(&Amounts::of("Z", 1)), None, "an absent unit is zero");
    assert!(Amounts::new(3, 5).covers(&Amounts::new(3, 0)));
    assert!(!Amounts::new(3, 5).covers(&Amounts::of("Z", 1)));
    assert!(Amounts::new(3, 5).covers(&Amounts::ZERO));
}

#[test]
fn a_lock_holding_any_unit_but_the_makers_is_inconsistent() {
    let terms = alice_sell_100();
    assert_eq!(terms.remaining_lots(&Amounts::of(UNIT_A, 6_000)), Ok(60));
    assert_eq!(terms.remaining_lots(&Amounts::new(6_000, 1)), Err(CoreError::InconsistentLock));
    let other = Amounts::of(UNIT_A, 6_000).checked_add(&Amounts::of("Z", 1)).unwrap();
    assert_eq!(terms.remaining_lots(&other), Err(CoreError::InconsistentLock));
}
