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

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
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
    /// Hang the link off this market's anchor instead of the escrow's own.
    #[serde(default)]
    pub anchor_market: Option<dex_core::MarketId>,
}

// ---------------------------------------------------------------------------
// Market orders
// ---------------------------------------------------------------------------

pub use dex_core::book::market::DEFAULT_MAX_SLIPPAGE_BPS;
pub use dex_core::book::MarketPlan;

/// How much a market order takes: whole lots, or a budget of the paying
/// asset (B for a buy, A for a sell) spent on whole lots.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub enum MarketAmount {
    Lots(u64),
    Budget(u64),
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct MarketPreviewRequest {
    /// The taker's direction: `Buy` sweeps asks, `Sell` sweeps bids.
    pub side: Side,
    pub amount: MarketAmount,
    /// Defaults to `DEFAULT_MAX_SLIPPAGE_BPS` (200 = 2%).
    pub max_slippage_bps: Option<u32>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct MarketOrderRequest {
    pub side: Side,
    pub lots: u64,
    pub max_slippage_bps: Option<u32>,
    /// The preview the user confirmed, if any. The result lists every order
    /// whose lots differ from it.
    pub expected: Option<MarketPlan<ActionHash>>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct MarketBudgetRequest {
    pub side: Side,
    /// Minor units of the paying asset: B for a buy, A for a sell.
    pub budget: u64,
    pub max_slippage_bps: Option<u32>,
    pub expected: Option<MarketPlan<ActionHash>>,
}

/// An order whose planned lots differ from the confirmed preview.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct FillChange {
    pub order: ActionHash,
    /// `0` if the preview did not include this order.
    pub expected_lots: u64,
    /// `0` if the order is no longer in the plan (filled, cancelled, expired).
    pub planned_lots: u64,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct MarketResult {
    pub plan: MarketPlan<ActionHash>,
    pub parks: Vec<PlacedPark>,
    /// Differences from `expected`; empty if none was given or nothing changed.
    pub changes: Vec<FillChange>,
    /// `0` for a market order, `1` for its one retry.
    pub attempt: u32,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct RetryMarketRequest {
    pub original: MarketResult,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct MarketRetry {
    /// Lots known to be unfilled: the original shortfall plus every settled
    /// park's unfilled lots.
    pub unfilled_lots: u64,
    /// Original parks the makers have not run yet; their lots are not retried.
    pub still_pending: u64,
    /// The retry, if there was anything to retry.
    pub result: Option<MarketResult>,
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
