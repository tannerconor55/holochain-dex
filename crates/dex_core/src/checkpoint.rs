//! # Balance checkpoints
//!
//! A checkpoint holds an author's cumulative ledger totals and every
//! allocation they have ever collected. Debit validation reads the latest
//! checkpoint and walks only the actions after it, instead of the whole
//! chain. Design: `docs/design/taker-protection.md` section 7.
//!
//! A checkpoint is valid when it equals the previous checkpoint extended by
//! the ledger entries between them; [`CheckpointState::extend`] is that
//! computation, used by validation and the coordinator alike.

use crate::Amounts;
use serde::{Deserialize, Serialize};

/// The coordinator writes a checkpoint once this many ledger entries follow
/// the last one. About 3 actions per ledger entry (entry plus links) keeps a
/// segment near 100 actions, well inside [`MAX_ACTIONS_SINCE_CHECKPOINT`].
pub const CHECKPOINT_EVERY: usize = 32;

/// ... or once this many actions follow it, whichever comes first. A run is
/// one ledger entry but up to 22 actions (its entry plus a link per receiver),
/// so counting entries alone could overrun the hard limit; half of it leaves
/// room for a whole zome call's writes after the check.
pub const CHECKPOINT_AFTER_ACTIONS: usize = MAX_ACTIONS_SINCE_CHECKPOINT / 2;

/// A debit, collect or reclaim is invalid when more actions than this follow
/// the checkpoint it cites, so skipping checkpoints cannot make validators
/// walk without bound. A Checkpoint itself is exempt, so an author past the
/// limit can always recover by writing one.
pub const MAX_ACTIONS_SINCE_CHECKPOINT: usize = 256;

/// Cumulative since genesis, per unit. Normalised like every `Amounts`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LedgerTotals {
    pub minted: Amounts,
    pub collected: Amounts,
    pub reclaimed: Amounts,
    /// Initial locks of the author's escrows.
    pub escrowed: Amounts,
    pub parked: Amounts,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckpointError {
    Overflow,
    /// Debits exceed credits.
    Overdrawn,
    /// An allocation collected twice.
    DuplicateCollect,
    /// The collected list is not sorted and unique.
    NotCanonical,
}

impl core::fmt::Display for CheckpointError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            CheckpointError::Overflow => write!(f, "ledger totals overflow"),
            CheckpointError::Overdrawn => write!(f, "debits exceed credits"),
            CheckpointError::DuplicateCollect => write!(f, "an allocation is collected twice"),
            CheckpointError::NotCanonical => write!(f, "collected allocations are not sorted and unique"),
        }
    }
}

impl std::error::Error for CheckpointError {}

impl LedgerTotals {
    /// Credits (minted + collected + reclaimed) minus debits (escrowed +
    /// parked), per unit. An overdrawn unit is an error.
    pub fn available(&self) -> Result<Amounts, CheckpointError> {
        let credits = self
            .minted
            .checked_add(&self.collected)
            .and_then(|c| c.checked_add(&self.reclaimed))
            .ok_or(CheckpointError::Overflow)?;
        let debits = self.escrowed.checked_add(&self.parked).ok_or(CheckpointError::Overflow)?;
        credits.checked_sub(&debits).ok_or(CheckpointError::Overdrawn)
    }
}

/// One ledger entry, as the checkpoint arithmetic sees it. `H` is the run's
/// action hash in the zome.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LedgerEvent<H> {
    Mint(Amounts),
    Collect { run: H, index: u32, amounts: Amounts },
    Reclaim(Amounts),
    /// An escrow's initial lock.
    Escrow(Amounts),
    Park(Amounts),
}

/// What a checkpoint records.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckpointState<H> {
    pub totals: LedgerTotals,
    /// Every `(run, allocation index)` ever collected, sorted and unique.
    /// Grows about 40 bytes per collect: a known limit (design doc section 7).
    pub collected: Vec<(H, u32)>,
}

impl<H: Ord + Clone> CheckpointState<H> {
    /// The state before an author's first ledger entry.
    pub fn genesis() -> Self {
        CheckpointState { totals: LedgerTotals::default(), collected: Vec::new() }
    }

    pub fn is_canonical(&self) -> bool {
        self.collected.windows(2).all(|w| w[0] < w[1])
    }

    pub fn is_collected(&self, run: &H, index: u32) -> bool {
        self.collected
            .binary_search_by(|(r, i)| (r, *i).cmp(&(run, index)))
            .is_ok()
    }

    /// This state followed by `events`, oldest first. Refuses a duplicate
    /// collect (against the state or within `events`) and any overflow. Does
    /// not refuse an overdraft: debit validation checks `available()` at the
    /// point of each debit.
    pub fn extend(&self, events: &[LedgerEvent<H>]) -> Result<Self, CheckpointError> {
        if !self.is_canonical() {
            return Err(CheckpointError::NotCanonical);
        }
        let mut next = self.clone();
        let add = |total: &Amounts, amount: &Amounts| total.checked_add(amount).ok_or(CheckpointError::Overflow);
        for event in events {
            let t = &mut next.totals;
            match event {
                LedgerEvent::Mint(a) => t.minted = add(&t.minted, a)?,
                LedgerEvent::Reclaim(a) => t.reclaimed = add(&t.reclaimed, a)?,
                LedgerEvent::Escrow(a) => t.escrowed = add(&t.escrowed, a)?,
                LedgerEvent::Park(a) => t.parked = add(&t.parked, a)?,
                LedgerEvent::Collect { run, index, amounts } => {
                    t.collected = add(&t.collected, amounts)?;
                    match next.collected.binary_search_by(|(r, i)| (r, *i).cmp(&(run, *index))) {
                        Ok(_) => return Err(CheckpointError::DuplicateCollect),
                        Err(at) => next.collected.insert(at, (run.clone(), *index)),
                    }
                }
            }
        }
        Ok(next)
    }
}

#[cfg(test)]
mod tests;
