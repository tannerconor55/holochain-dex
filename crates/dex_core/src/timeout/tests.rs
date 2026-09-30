use super::*;

const T: Timing = Timing { park_timeout_us: 1_800_000_000, settle_grace_us: 300_000_000 };

fn act(timestamp: i64) -> MakerAction {
    MakerAction { timestamp, consumes_park: false }
}

fn run(timestamp: i64) -> MakerAction {
    MakerAction { timestamp, consumes_park: true }
}

#[test]
fn the_deadline_is_timeout_or_expiry_whichever_first_plus_grace() {
    assert_eq!(park_deadline(1_000, i64::MAX / 2, &T), Some(1_000 + 1_800_000_000 + 300_000_000));
    assert_eq!(park_deadline(1_000, 5_000, &T), Some(5_000 + 300_000_000), "expiry first");
    assert_eq!(park_deadline(i64::MAX - 10, i64::MAX, &T), None, "overflow is refused");
}

#[test]
fn a_run_may_consume_only_before_the_deadline() {
    assert!(run_may_consume(99, 100));
    assert!(!run_may_consume(100, 100), "the deadline itself is too late");
    assert!(!run_may_consume(101, 100));
}

// The design doc's traces (section 4.2), with D = 100.

#[test]
fn t1_a_settled_park_cannot_be_reclaimed() {
    // escrow, run at 50, anchor at 120.
    let walk = [act(10), run(50), act(120)];
    assert_eq!(reclaim_valid(&walk, 100), Err(ReclaimRefusal::ConsumedByRun));
}

#[test]
fn t2_any_maker_action_after_the_deadline_lets_the_taker_reclaim() {
    assert_eq!(reclaim_valid(&[act(10), act(40), act(100)], 100), Ok(()));
}

#[test]
fn t3_a_run_after_the_anchor_is_invalid_however_it_is_stamped() {
    // After an anchor at 120 the chain only allows timestamps >= 120.
    let backdated_as_far_as_allowed = 120;
    assert!(!run_may_consume(backdated_as_far_as_allowed, 100));
}

#[test]
fn t4_a_run_raced_in_just_before_the_deadline_wins() {
    assert!(run_may_consume(99, 100));
    assert_eq!(reclaim_valid(&[act(10), run(99), act(100)], 100), Err(ReclaimRefusal::ConsumedByRun));
}

#[test]
fn t5_an_anchor_before_the_deadline_is_refused() {
    assert_eq!(reclaim_valid(&[act(10), act(99)], 100), Err(ReclaimRefusal::BeforeDeadline));
}

#[test]
fn no_anchor_and_disordered_chains_are_refused() {
    assert_eq!(reclaim_valid(&[], 100), Err(ReclaimRefusal::NoAnchor));
    assert_eq!(reclaim_valid(&[act(150), act(120)], 100), Err(ReclaimRefusal::NotChronological));
}

/// xorshift64: deterministic pseudo-randomness without a dependency.
struct Rng(u64);

impl Rng {
    fn next(&mut self, bound: u64) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0 % bound
    }
}

/// Generated maker chains: non-decreasing timestamps (the only rule
/// Holochain enforces, so every backdating it allows is covered), runs at
/// arbitrary positions, and a Reclaim tried at every possible anchor.
#[test]
fn at_most_one_of_run_and_reclaim_is_ever_valid() {
    let mut rng = Rng(0x0DDB_A11C_0FFE_E123);
    let mut with_run = 0;
    let mut reclaimable = 0;
    for _ in 0..20_000 {
        let deadline = 50 + rng.next(100) as i64;
        let mut ts = rng.next(120) as i64;
        // chain[0] is the escrow.
        let mut chain = vec![act(ts)];
        let mut consumed = false;
        for _ in 0..rng.next(12) {
            ts += rng.next(4) as i64; // equal timestamps are allowed
            let wants_to_run = rng.next(3) == 0;
            // Only what validation accepts can be on a chain: a run that may
            // consume the park, and only the first one.
            if wants_to_run && !consumed && run_may_consume(ts, deadline) {
                chain.push(run(ts));
                consumed = true;
            } else {
                chain.push(act(ts));
            }
        }
        let run_valid = chain.iter().any(|a| a.consumes_park);
        let any_reclaim_valid = (0..chain.len()).any(|k| reclaim_valid(&chain[..=k], deadline).is_ok());
        assert!(!(run_valid && any_reclaim_valid), "both valid: {chain:?} deadline {deadline}");
        // Liveness: with no run, any action at or after the deadline anchors a reclaim.
        let has_late_action = chain.iter().any(|a| a.timestamp >= deadline);
        assert_eq!(!run_valid && has_late_action, any_reclaim_valid, "{chain:?} deadline {deadline}");
        with_run += usize::from(run_valid);
        reclaimable += usize::from(any_reclaim_valid);
    }
    assert!(with_run > 2_000 && reclaimable > 2_000, "both outcomes are exercised: {with_run} / {reclaimable}");
}
