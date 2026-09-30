//! # dex_api
//!
//! The input and output types of the `dex` zome's externs. Field names are
//! part of the wire format (msgpack maps are keyed by name).

use hdi::prelude::{ActionHash, AgentPubKey};
use dex_core::{MarketDef, MarketId};
use ledger_api::{EscrowState, OrderTerms, PendingPark, RunMode, Side};
use serde::{Deserialize, Serialize};

pub use dex_core::book::{BookView, OrderStatus, OrderView, PlannedFill, PriceLevel, TakePlan};

/// A listed order as the book sees it.
pub type Order = OrderView<ActionHash, AgentPubKey>;

/// A market declared in the DNA properties, with its units' decimals for display.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct MarketInfo {
    pub id: MarketId,
    pub def: MarketDef,
    pub base_decimals: u8,
    pub quote_decimals: u8,
}

/// What the UI needs from the DNA properties.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct DexConfig {
    /// A park's run window after it is written (before the order's expiry
    /// cuts it short), then the settle grace; see `dex_core::timeout`.
    pub park_timeout_secs: u64,
    pub settle_grace_secs: u64,
    /// In declaration order; the first is the default.
    pub markets: Vec<MarketInfo>,
}

// Every `market` below is optional: `None` means the first market the DNA
// properties declare.

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct PlaceOrderRequest {
    #[serde(default)]
    pub market: Option<MarketId>,
    pub terms: OrderTerms,
}

/// One price level on one side of the book.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct LevelQuery {
    #[serde(default)]
    pub market: Option<MarketId>,
    /// The makers' side: `Sell` for asks, `Buy` for bids.
    pub side: Side,
    pub price_per_lot: u64,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct TakeRequest {
    #[serde(default)]
    pub market: Option<MarketId>,
    /// The taker's direction: `Buy` takes asks, `Sell` takes bids.
    pub take: Side,
    pub lots: u64,
    /// Worst price accepted, in quote (HF) minor units per lot.
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
// Trade history and price data (derived from settlement runs; see dex_trades)
// ---------------------------------------------------------------------------

pub use dex_trades::{Candle, CandleInterval, MarketStats, MAX_CANDLES};

/// One trade as the book reports it.
pub type Trade = dex_trades::Trade<ActionHash>;

/// Most trades `get_recent_trades` returns.
pub const MAX_RECENT_TRADES: u32 = 200;

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct RecentTradesRequest {
    #[serde(default)]
    pub market: Option<MarketId>,
    /// At most `MAX_RECENT_TRADES`.
    pub limit: u32,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct CandlesRequest {
    #[serde(default)]
    pub market: Option<MarketId>,
    pub interval: CandleInterval,
    /// µs since the epoch; `from` inclusive, `to` exclusive.
    pub from: i64,
    pub to: i64,
}

// ---------------------------------------------------------------------------
// Maker presence
// ---------------------------------------------------------------------------

/// Whether an order's maker answered a ping just now. Advisory only: a maker
/// can go offline right after answering, and one who did not answer may be
/// back before the park's deadline. Nothing in validation depends on it.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct MakerPresence {
    pub order: ActionHash,
    pub maker: AgentPubKey,
    pub reachable: bool,
    /// Why not, when not reachable.
    pub detail: Option<String>,
}

// ---------------------------------------------------------------------------
// Market orders
// ---------------------------------------------------------------------------

pub use dex_core::book::market::DEFAULT_MAX_SLIPPAGE_BPS;
pub use dex_core::book::MarketPlan;

/// How much a market order takes: whole lots, or a budget of the paying
/// asset (quote for a buy, base for a sell) spent on whole lots.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub enum MarketAmount {
    Lots(u64),
    Budget(u64),
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct MarketPreviewRequest {
    #[serde(default)]
    pub market: Option<MarketId>,
    /// The taker's direction: `Buy` sweeps asks, `Sell` sweeps bids.
    pub side: Side,
    pub amount: MarketAmount,
    /// Defaults to `DEFAULT_MAX_SLIPPAGE_BPS` (200 = 2%).
    pub max_slippage_bps: Option<u32>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct MarketOrderRequest {
    #[serde(default)]
    pub market: Option<MarketId>,
    pub side: Side,
    pub lots: u64,
    pub max_slippage_bps: Option<u32>,
    /// The preview the user confirmed, if any. The result lists every order
    /// whose lots differ from it.
    pub expected: Option<MarketPlan<ActionHash>>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct MarketBudgetRequest {
    #[serde(default)]
    pub market: Option<MarketId>,
    pub side: Side,
    /// Minor units of the paying asset: quote for a buy, base for a sell.
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
    /// The market it swept; a retry sweeps the same one.
    pub market: MarketId,
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

/// How one of the recipient's parks came out of a run.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct SignalFill {
    pub park: ActionHash,
    /// `0`: refunded in full.
    pub filled_lots: u64,
    pub requested_lots: u64,
}

/// Sent between agents' dex zomes and re-emitted to the local UI, or emitted
/// to the local UI only (`OrderUpdated`).
///
/// Signals are hints, never state: they can be lost, delayed or forged. A
/// client reacts by re-reading and calling idempotent externs
/// (`run_my_orders`, `collect_all`), and also polls, so a missed signal only
/// delays settlement or a notification, never loses it. Each carries the
/// stable ids (park, run, escrow and status) a client needs to dedupe it
/// against what its next poll shows.
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
        /// The recipient's own parks this run consumed.
        #[serde(default)]
        fills: Vec<SignalFill>,
    },
    /// The recipient's own order after a run they wrote (local only).
    OrderUpdated {
        escrow: ActionHash,
        run: ActionHash,
        status: OrderStatus,
        filled_lots: u64,
        lots: u64,
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
                fills: vec![SignalFill { park: action(2), filled_lots: 10, requested_lots: 40 }],
            },
            "run_settled",
        );
    }

    #[test]
    fn order_updated_is_tagged_and_round_trips() {
        round_trip(
            DexSignal::OrderUpdated {
                escrow: action(1),
                run: action(4),
                status: OrderStatus::Partial,
                filled_lots: 10,
                lots: 40,
            },
            "order_updated",
        );
    }
}
