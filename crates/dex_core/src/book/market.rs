//! # Market orders
//!
//! A market order is a taker action only: sweep resting orders from the best
//! price, within a slippage limit, and park against each. It never rests on
//! the book (immediate-or-cancel); whatever the makers' runs cannot fill is
//! refunded by them. Each maker settles at their own limit price.
//!
//! Everything here is built on [`plan_take`]: a market plan is a take plan
//! whose limit price is derived from the best price the taker can hit and a
//! slippage allowance in basis points. Integer maths only.

use super::{plan_take, side_in_priority, OrderView, TakePlan};
use crate::{Amounts, Side};
use serde::{Deserialize, Serialize};

/// Basis points in 100%.
pub const BPS: u64 = 10_000;

/// The default slippage allowance: 200 bps = 2%.
pub const DEFAULT_MAX_SLIPPAGE_BPS: u32 = 200;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MarketError {
    /// No live order on the other side that this taker may take.
    EmptyBook,
    /// Zero lots, or a budget too small for one lot at the best price.
    NothingToTake,
    /// A sell allowance of more than 100% would give a negative limit.
    SlippageTooLarge,
    Overflow,
}

impl core::fmt::Display for MarketError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            MarketError::EmptyBook => write!(f, "no orders on the other side of the book to take"),
            MarketError::NothingToTake => write!(f, "nothing to take: zero lots, or a budget below one lot"),
            MarketError::SlippageTooLarge => write!(f, "slippage allowance above 10000 bps (100%)"),
            MarketError::Overflow => write!(f, "arithmetic overflow"),
        }
    }
}

impl std::error::Error for MarketError {}

/// A planned market order: the take plan plus how it was derived and what it
/// averages to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MarketPlan<P> {
    /// The taker's direction: `Buy` sweeps asks, `Sell` sweeps bids.
    pub take: Side,
    pub plan: TakePlan<P>,
    /// The best price this taker could hit when planning; the limit derives
    /// from it. `None` when re-planning against a book with nothing to hit.
    pub reference_price: Option<u64>,
    /// `None` when the limit was given rather than derived (a retry).
    pub max_slippage_bps: Option<u32>,
    /// Worst price accepted, UNIT-B minor units per lot.
    pub limit_price: u64,
    /// Average price as an exact rational: `total_quote_minor / total_lots`
    /// UNIT-B minor units per lot. Both are zero when nothing is planned.
    pub total_quote_minor: u64,
    pub total_lots: u64,
    /// The least favourable price in the plan (highest for a buy, lowest for
    /// a sell).
    pub worst_price: Option<u64>,
    /// For a budget plan: the part of the budget the plan does not spend.
    pub unspent_budget: Option<u64>,
}

fn maker_side(take: Side) -> Side {
    match take {
        Side::Buy => Side::Sell,
        Side::Sell => Side::Buy,
    }
}

/// The best price `taker` can hit: live orders on the other side, not their own.
pub fn best_price<P: Ord, K: PartialEq>(
    orders: &[OrderView<P, K>],
    taker: &K,
    take: Side,
    now: i64,
) -> Option<u64> {
    side_in_priority(orders, maker_side(take), now)
        .into_iter()
        .find(|o| &o.maker != taker)
        .map(|o| o.price_per_lot)
}

/// The limit price for a slippage allowance:
/// buy `ceil(best × (10000 + bps) / 10000)`, sell `floor(best × (10000 − bps) / 10000)`.
pub fn market_limit(take: Side, best: u64, max_slippage_bps: u32) -> Result<u64, MarketError> {
    let best = u128::from(best);
    let bps = u128::from(max_slippage_bps);
    let scale = u128::from(BPS);
    let limit = match take {
        Side::Buy => (best * (scale + bps)).div_ceil(scale),
        Side::Sell => {
            let keep = scale.checked_sub(bps).ok_or(MarketError::SlippageTooLarge)?;
            best * keep / scale
        }
    };
    u64::try_from(limit).map_err(|_| MarketError::Overflow)
}

/// Sweep `lots` from the best price, stopping at the slippage limit.
pub fn plan_market<P: Ord + Clone, K: PartialEq>(
    orders: &[OrderView<P, K>],
    take: Side,
    lots: u64,
    max_slippage_bps: u32,
    taker: &K,
    now: i64,
) -> Result<MarketPlan<P>, MarketError> {
    if lots == 0 {
        return Err(MarketError::NothingToTake);
    }
    let best = best_price(orders, taker, take, now).ok_or(MarketError::EmptyBook)?;
    let limit = market_limit(take, best, max_slippage_bps)?;
    let mut market = plan_with_limit(orders, take, lots, limit, taker, now)?;
    market.reference_price = Some(best);
    market.max_slippage_bps = Some(max_slippage_bps);
    Ok(market)
}

/// Spend at most `budget_minor` of the taker's paying asset (B for a buy,
/// A for a sell) on whole lots, from the best price, within the slippage
/// limit. Never exceeds the budget.
pub fn plan_market_by_budget<P: Ord + Clone, K: PartialEq>(
    orders: &[OrderView<P, K>],
    take: Side,
    budget_minor: u64,
    max_slippage_bps: u32,
    taker: &K,
    now: i64,
) -> Result<MarketPlan<P>, MarketError> {
    let best = best_price(orders, taker, take, now).ok_or(MarketError::EmptyBook)?;
    let limit = market_limit(take, best, max_slippage_bps)?;

    // How many lots the budget buys, walking the same orders in the same
    // priority `plan_take` will. `plan_take` fills whole orders until the
    // last, so asking it for exactly these lots reproduces this walk.
    let mut left = budget_minor;
    let mut lots: u64 = 0;
    for order in side_in_priority(orders, maker_side(take), now) {
        if !within(take, order.price_per_lot, limit) {
            break;
        }
        if &order.maker == taker {
            continue;
        }
        let per_lot = order.remaining_terms().taker_units_per_lot();
        let fill = order.remaining_lots.min(left / per_lot);
        if fill == 0 {
            break; // later orders cost at least as much per lot
        }
        left -= fill * per_lot; // fill × per_lot ≤ left
        lots += fill;
    }
    if lots == 0 {
        return Err(MarketError::NothingToTake);
    }
    let mut market = plan_with_limit(orders, take, lots, limit, taker, now)?;
    let spent = pay(take, &market.plan.total_cost);
    market.reference_price = Some(best);
    market.max_slippage_bps = Some(max_slippage_bps);
    market.unspent_budget = Some(budget_minor.checked_sub(spent).ok_or(MarketError::Overflow)?);
    Ok(market)
}

/// Plan `lots` against a given limit price. Used for retries, which keep the
/// original order's limit rather than deriving a new one from a moved book.
/// An empty book is not an error here: the plan simply fills nothing.
pub fn plan_with_limit<P: Ord + Clone, K: PartialEq>(
    orders: &[OrderView<P, K>],
    take: Side,
    lots: u64,
    limit_price: u64,
    taker: &K,
    now: i64,
) -> Result<MarketPlan<P>, MarketError> {
    let plan = plan_take(orders, taker, take, lots, Some(limit_price), now).map_err(|_| MarketError::Overflow)?;
    let mut total_quote_minor: u64 = 0;
    for fill in &plan.fills {
        let quote = fill.lots.checked_mul(fill.price_per_lot).ok_or(MarketError::Overflow)?;
        total_quote_minor = total_quote_minor.checked_add(quote).ok_or(MarketError::Overflow)?;
    }
    // Fills come best price first, so the last is the worst.
    let worst_price = plan.fills.last().map(|f| f.price_per_lot);
    Ok(MarketPlan {
        take,
        total_lots: plan.filled,
        plan,
        reference_price: best_price(orders, taker, take, now),
        max_slippage_bps: None,
        limit_price,
        total_quote_minor,
        worst_price,
        unspent_budget: None,
    })
}

fn within(take: Side, price: u64, limit: u64) -> bool {
    match take {
        Side::Buy => price <= limit,
        Side::Sell => price >= limit,
    }
}

/// The paying asset's amount: HF for a buy, A for a sell.
fn pay(take: Side, amounts: &Amounts) -> u64 {
    match take {
        Side::Buy => amounts.get(crate::HUB_UNIT),
        Side::Sell => amounts.get(crate::UNIT_A),
    }
}

#[cfg(test)]
mod tests;
