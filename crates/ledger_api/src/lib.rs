//! # ledger_api
//!
//! The input and output types of the `ledger` zome's externs. This is the
//! settlement interface's data contract: the ledger coordinator implements it,
//! and the dex coordinator and the Sweettest suite call it. Field names are
//! part of the wire format (msgpack maps are keyed by name).

use hdi::prelude::{ActionHash, AgentPubKey, Timestamp};
use serde::{Deserialize, Serialize};

pub use dex_core::{Amounts, OrderTerms, RunMode, Side};
pub use entries::{Escrow, ESCROW_ENTRY_INDEX, LEDGER_INTEGRITY_ZOME};

/// Ledger entries that other integrity zomes must decode. Defined here, not in
/// `ledger_integrity`, because depending on that crate would link its
/// `validate` and `entry_defs` exports into the dependent zome's wasm.
mod entries {
    use hdi::prelude::*;

    /// The ledger's integrity zome name in `dna.yaml`.
    pub const LEDGER_INTEGRITY_ZOME: &str = "ledger_integrity";

    /// `Escrow`'s position in `ledger_integrity::EntryTypes`. That crate
    /// asserts this at compile time, so reordering its variants cannot
    /// silently break zomes that check it.
    pub const ESCROW_ENTRY_INDEX: u8 = 1;

    /// An order's escrow. Creating it locks `terms.initial_lock(market)` from the maker.
    /// The action hash of this entry is the order's identity.
    #[hdk_entry_helper]
    #[derive(Clone, PartialEq)]
    pub struct Escrow {
        /// The order's market; must be declared in the DNA properties.
        pub market: dex_core::MarketId,
        pub terms: dex_core::OrderTerms,
        /// The maker's latest checkpoint when opening (the lock is a debit).
        pub checkpoint: Option<ActionHash>,
    }
}

/// Open an order's escrow in a market declared in the DNA properties.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct OpenEscrowRequest {
    pub market: dex_core::MarketId,
    pub terms: OrderTerms,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct ParkRequest {
    pub escrow: ActionHash,
    pub amounts: Amounts,
    pub requested_lots: u64,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct RunEscrowInput {
    pub escrow: ActionHash,
    pub mode: RunMode,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct ParkFill {
    pub park: ActionHash,
    pub filled_lots: u64,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct RunReport {
    pub run: ActionHash,
    pub mode: RunMode,
    pub filled_lots: u64,
    pub fills: Vec<ParkFill>,
    pub locked: Amounts,
    /// Parks still waiting because this run hit the per-run cap.
    pub still_pending: usize,
    /// Every agent this run allocated funds to (takers and the maker).
    pub receivers: Vec<AgentPubKey>,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct EscrowState {
    pub escrow: ActionHash,
    pub maker: AgentPubKey,
    pub market: dex_core::MarketId,
    pub terms: OrderTerms,
    pub opened_at: Timestamp,
    pub locked: Amounts,
    pub remaining_lots: u64,
    pub filled_lots: u64,
    pub runs: usize,
    pub expired: bool,
    /// Released: nothing left locked and no further fills possible.
    pub closed: bool,
    /// Timestamp of the first `Release` run, if the order was released.
    pub released_at: Option<Timestamp>,
}

/// One of the caller's parks and how it resolved.
#[derive(Serialize, Deserialize, Debug)]
pub struct ParkStatus {
    pub park: ActionHash,
    pub escrow: ActionHash,
    pub amounts: Amounts,
    pub requested_lots: u64,
    pub parked_at: Timestamp,
    /// `None` while the park waits for the maker's next run.
    pub settlement: Option<ParkSettlement>,
    /// After this, no run may consume the park; the taker may reclaim it once
    /// the maker has written anything at or after it.
    pub deadline: Timestamp,
    /// The Reclaim that took the park back, if any.
    pub reclaimed: Option<ActionHash>,
    /// Unsettled, unreclaimed, past its deadline, and the maker has written an
    /// action since: `reclaim_park` will succeed (unless a run is still
    /// gossiping in).
    pub reclaimable: bool,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct ParkSettlement {
    /// The run that consumed the park.
    pub run: ActionHash,
    /// Lots filled; `0` means the park was refunded in full.
    pub filled_lots: u64,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct PendingPark {
    pub park: ActionHash,
    pub taker: AgentPubKey,
    pub amounts: Amounts,
    pub requested_lots: u64,
    pub parked_at: Timestamp,
}

/// §23 of the protocol: total = available + reserved.
#[derive(Serialize, Deserialize, Debug)]
pub struct BalanceView {
    /// Spendable now.
    pub available: Amounts,
    /// Held in this agent's own open escrows.
    pub locked_in_escrows: Amounts,
    /// Parked against other makers' orders and not yet settled.
    pub parked: Amounts,
    /// Settled to this agent but not yet collected.
    pub uncollected: Amounts,
    pub total: Amounts,
}

// ---------------------------------------------------------------------------
// Test-only requests
// ---------------------------------------------------------------------------
//
// These let a Sweettest write what the coordinator never would, to prove
// validation refuses it. None can create an entry validation accepts that
// the normal externs could not.

/// A park with every field chosen by the caller.
#[derive(Serialize, Deserialize, Debug)]
pub struct RawPark {
    pub escrow: ActionHash,
    pub market: dex_core::MarketId,
    pub amounts: Amounts,
    pub requested_lots: u64,
    pub checkpoint: Option<ActionHash>,
}

/// A reclaim citing any anchor, without the coordinator's checks.
#[derive(Serialize, Deserialize, Debug)]
pub struct RawReclaim {
    pub park: ActionHash,
    pub anchor: ActionHash,
}

/// A checkpoint with any contents.
#[derive(Serialize, Deserialize, Debug)]
pub struct RawCheckpoint {
    pub prev: Option<ActionHash>,
    pub state: dex_core::checkpoint::CheckpointState<ActionHash>,
}

/// One of the caller's checkpoints.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct CheckpointView {
    pub checkpoint: ActionHash,
    pub state: dex_core::checkpoint::CheckpointState<ActionHash>,
}
