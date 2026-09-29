//! # ledger_api
//!
//! The input and output types of the `ledger` zome's externs. This is the
//! settlement interface's data contract: the ledger coordinator implements it,
//! and the dex coordinator and the Sweettest suite call it. Field names are
//! part of the wire format (msgpack maps are keyed by name).

use hdi::prelude::{ActionHash, AgentPubKey, Timestamp};
use serde::{Deserialize, Serialize};

pub use dex_core::{Amounts, OrderTerms, RunMode, Side};

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
}

#[derive(Serialize, Deserialize, Debug)]
pub struct EscrowState {
    pub escrow: ActionHash,
    pub maker: AgentPubKey,
    pub terms: OrderTerms,
    pub opened_at: Timestamp,
    pub locked: Amounts,
    pub remaining_lots: u64,
    pub filled_lots: u64,
    pub runs: usize,
    pub expired: bool,
    /// Released: nothing left locked and no further fills possible.
    pub closed: bool,
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
