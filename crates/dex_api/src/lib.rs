//! # dex_api
//!
//! The input and output types of the `dex` zome's externs. Field names are
//! part of the wire format (msgpack maps are keyed by name).

use hdi::prelude::{ActionHash, AgentPubKey};
use ledger_api::{EscrowState, PendingPark, Side};
use serde::{Deserialize, Serialize};

pub use dex_core::book::{BookView, OrderStatus, OrderView, PlannedFill, PriceLevel, TakePlan};

/// A listed order as the book sees it.
pub type Order = OrderView<ActionHash, AgentPubKey>;

/// One price level on one side of the book.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct LevelQuery {
    /// The makers' side: `Sell` for asks, `Buy` for bids.
    pub side: Side,
    pub price_per_lot: u64,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct TakeRequest {
    /// The taker's direction: `Buy` takes asks, `Sell` takes bids.
    pub take: Side,
    pub lots: u64,
    /// Worst price accepted, in UNIT-B minor units per lot.
    pub limit_price: Option<u64>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct PlacedPark {
    pub escrow: ActionHash,
    pub park: ActionHash,
    pub lots: u64,
}

/// What `take` did. `plan` is re-derived from fresh state, so it can differ
/// from an earlier `plan_take` preview; compare them to show what changed.
#[derive(Serialize, Deserialize, Debug)]
pub struct TakeResult {
    pub plan: TakePlan<ActionHash>,
    pub parks: Vec<PlacedPark>,
}

/// A listing with a caller-chosen tag. Only for testing validation, which
/// rejects every tag except the escrow's own.
#[derive(Serialize, Deserialize, Debug)]
pub struct RawListing {
    pub escrow: ActionHash,
    pub tag: Vec<u8>,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct MyOrder {
    pub state: EscrowState,
    /// Parks waiting for this order's next run, oldest first.
    pub pending: Vec<PendingPark>,
    pub status: OrderStatus,
}
