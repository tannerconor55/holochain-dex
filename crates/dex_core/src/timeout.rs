//! # Park timeout: the run-versus-reclaim rule
//!
//! The safety property: for any park, at most one of (1) a settlement run
//! that consumes it and (2) a Reclaim of it is ever valid. Design:
//! `docs/design/taker-protection.md` section 4.2.
//!
//! Holochain lets an author backdate an action to its own previous action and
//! no further (timestamps are non-decreasing along a chain; nothing else is
//! checked). So:
//!
//! - a run may consume a park only while `run.timestamp < deadline`;
//! - a Reclaim cites an *anchor*, any maker action stamped `>= deadline`, and
//!   the maker's chain from the escrow up to that anchor must hold no run
//!   consuming the park. Every later maker action is stamped `>=` the anchor,
//!   so no later run can consume it either.
//!
//! These are the functions `ledger_integrity` calls after reading the chain,
//! and the ones the property test drives, so both exercise one rule.

use crate::properties::Timing;

/// The last moment (exclusive) a run may consume a park:
/// `min(parked_at + park_timeout, order expiry) + settle_grace`.
/// `None` on overflow.
pub fn park_deadline(parked_at: i64, expires_at: i64, timing: &Timing) -> Option<i64> {
    let timeout = parked_at.checked_add(timing.park_timeout_us)?;
    timeout.min(expires_at).checked_add(timing.settle_grace_us)
}

/// Whether a run stamped `run_ts` may consume a park with this deadline.
pub fn run_may_consume(run_ts: i64, deadline: i64) -> bool {
    run_ts < deadline
}

/// One action on the maker's chain, as the reclaim rule sees it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MakerAction {
    pub timestamp: i64,
    /// A settlement run whose consumed parks include this park.
    pub consumes_park: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReclaimRefusal {
    /// The walk is empty: there is no anchor.
    NoAnchor,
    /// The anchor is stamped before the deadline.
    BeforeDeadline,
    /// A run between the escrow and the anchor consumed the park.
    ConsumedByRun,
    /// Timestamps decrease along the walk, which system validation forbids;
    /// refused rather than trusted.
    NotChronological,
}

impl core::fmt::Display for ReclaimRefusal {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            ReclaimRefusal::NoAnchor => write!(f, "no anchor action on the maker's chain"),
            ReclaimRefusal::BeforeDeadline => write!(f, "the anchor is stamped before the park's deadline"),
            ReclaimRefusal::ConsumedByRun => write!(f, "a settlement run consumed the park before the anchor"),
            ReclaimRefusal::NotChronological => write!(f, "the maker's chain is not in time order"),
        }
    }
}

/// Whether a Reclaim is valid. `walk` is the maker's chain from the escrow
/// action up to and including the anchor, oldest first; the anchor is its
/// last action. The walk must reach the escrow: a run between the escrow and
/// a shorter walk's start would be missed.
pub fn reclaim_valid(walk: &[MakerAction], deadline: i64) -> Result<(), ReclaimRefusal> {
    let anchor = walk.last().ok_or(ReclaimRefusal::NoAnchor)?;
    if walk.windows(2).any(|w| w[1].timestamp < w[0].timestamp) {
        return Err(ReclaimRefusal::NotChronological);
    }
    if anchor.timestamp < deadline {
        return Err(ReclaimRefusal::BeforeDeadline);
    }
    if walk.iter().any(|a| a.consumes_park) {
        return Err(ReclaimRefusal::ConsumedByRun);
    }
    Ok(())
}

#[cfg(test)]
mod tests;
