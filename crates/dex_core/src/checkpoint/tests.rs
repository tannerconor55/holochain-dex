use super::*;

type State = CheckpointState<u32>;

fn a(n: u64) -> Amounts {
    Amounts::new(n, 0)
}

#[test]
fn totals_accumulate_and_available_is_credits_minus_debits() {
    let s = State::genesis()
        .extend(&[
            LedgerEvent::Mint(Amounts::new(1_000, 500)),
            LedgerEvent::Escrow(a(400)),
            LedgerEvent::Park(Amounts::new(0, 200)),
            LedgerEvent::Collect { run: 7, index: 0, amounts: a(50) },
            LedgerEvent::Reclaim(Amounts::new(0, 200)),
        ])
        .unwrap();
    assert_eq!(s.totals.available(), Ok(Amounts::new(650, 500)));
    assert_eq!(s.collected, vec![(7, 0)]);
}

#[test]
fn extending_in_two_steps_equals_one_step() {
    let events = [
        LedgerEvent::Mint(a(10)),
        LedgerEvent::Collect { run: 3, index: 1, amounts: a(5) },
        LedgerEvent::Collect { run: 1, index: 0, amounts: a(5) },
        LedgerEvent::Park(a(4)),
    ];
    let once = State::genesis().extend(&events).unwrap();
    let twice = State::genesis().extend(&events[..2]).unwrap().extend(&events[2..]).unwrap();
    assert_eq!(once, twice);
    assert!(once.is_canonical());
    assert_eq!(once.collected, vec![(1, 0), (3, 1)], "sorted");
}

#[test]
fn a_duplicate_collect_is_refused_across_and_within_segments() {
    let first = State::genesis().extend(&[LedgerEvent::Collect { run: 9, index: 2, amounts: a(1) }]).unwrap();
    assert!(first.is_collected(&9, 2));
    assert!(!first.is_collected(&9, 3));
    assert_eq!(
        first.extend(&[LedgerEvent::Collect { run: 9, index: 2, amounts: a(1) }]),
        Err(CheckpointError::DuplicateCollect),
        "collected before the checkpoint"
    );
    assert_eq!(
        State::genesis().extend(&[
            LedgerEvent::Collect { run: 1, index: 0, amounts: a(1) },
            LedgerEvent::Collect { run: 1, index: 0, amounts: a(1) },
        ]),
        Err(CheckpointError::DuplicateCollect),
        "twice in one segment"
    );
}

#[test]
fn overdraft_overflow_and_non_canonical_states_are_refused() {
    let over = State::genesis().extend(&[LedgerEvent::Mint(a(5)), LedgerEvent::Park(a(6))]).unwrap();
    assert_eq!(over.totals.available(), Err(CheckpointError::Overdrawn));
    let max = State::genesis().extend(&[LedgerEvent::Mint(a(u64::MAX))]).unwrap();
    assert_eq!(max.extend(&[LedgerEvent::Mint(a(1))]), Err(CheckpointError::Overflow));
    let bad = State { totals: LedgerTotals::default(), collected: vec![(2, 0), (1, 0)] };
    assert!(!bad.is_canonical());
    assert_eq!(bad.extend(&[]), Err(CheckpointError::NotCanonical));
}
