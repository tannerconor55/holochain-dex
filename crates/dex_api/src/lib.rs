//! # dex_api
//!
//! The input and output types of the `dex` zome's externs. Field names are
//! part of the wire format (msgpack maps are keyed by name).

use hdi::prelude::{ActionHash, AgentPubKey};
use ledger_api::{EscrowState, PendingPark, RunMode, Side};
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

/// Sent between agents' dex zomes and re-emitted to the local UI.
///
/// Signals are hints, never state: they can be lost, delayed or forged. A
/// client reacts by re-reading and calling idempotent externs
/// (`run_my_orders`, `collect_all`), and also polls, so a missed signal only
/// delays settlement.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum DexSignal {
    /// A taker parked funds against the recipient's order. The recipient's
    /// client should call `run_my_orders`.
    ParkPlaced {
        escrow: ActionHash,
        park: ActionHash,
        taker: AgentPubKey,
        lots: u64,
    },
    /// A maker's run allocated funds to the recipient. The recipient's client
    /// should call the ledger's `collect_all`.
    RunSettled {
        escrow: ActionHash,
        run: ActionHash,
        maker: AgentPubKey,
        mode: RunMode,
    },
}

#[derive(Serialize, Deserialize, Debug)]
pub struct MyOrder {
    pub state: EscrowState,
    /// Parks waiting for this order's next run, oldest first.
    pub pending: Vec<PendingPark>,
    pub status: OrderStatus,
}

#[cfg(test)]
mod tests {
    use super::*;
    use hdi::prelude::{decode, encode};

    fn action(byte: u8) -> ActionHash {
        ActionHash::from_raw_36(vec![byte; 36])
    }

    fn agent(byte: u8) -> AgentPubKey {
        AgentPubKey::from_raw_36(vec![byte; 36])
    }

    /// The part of the wire shape a UI switches on.
    #[derive(Deserialize, Debug)]
    struct Tagged {
        #[serde(rename = "type")]
        kind: String,
    }

    fn round_trip(signal: DexSignal, kind: &str) {
        let bytes = encode(&signal).unwrap();
        assert_eq!(decode::<_, DexSignal>(&bytes).unwrap(), signal);
        assert_eq!(decode::<_, Tagged>(&bytes).unwrap().kind, kind);
    }

    #[test]
    fn park_placed_is_tagged_and_round_trips() {
        round_trip(
            DexSignal::ParkPlaced {
                escrow: action(1),
                park: action(2),
                taker: agent(3),
                lots: 40,
            },
            "park_placed",
        );
    }

    #[test]
    fn run_settled_is_tagged_and_round_trips() {
        round_trip(
            DexSignal::RunSettled {
                escrow: action(1),
                run: action(4),
                maker: agent(5),
                mode: RunMode::Fill,
            },
            "run_settled",
        );
    }
}
