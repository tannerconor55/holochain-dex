//! # Order book views
//!
//! Pure functions that turn a set of orders into what the UI shows and what a
//! taker should park. Nothing here is stored: the book is a view over ledger
//! state (§10), recomputed on every read.
//!
//! The quantity of every order comes from the ledger only (the latest run's
//! `locked`, surfaced as [`OrderView::remaining_lots`]). This module never
//! derives remaining quantity itself.
//!
//! Priority is price first, then `(opened_at, id)` ascending: the same
//! tiebreak [`crate::execute_run`] uses for parks.

use crate::{Amounts, CoreError, OrderTerms, Side};
use std::cmp::Ordering;

/// One order as the book sees it. `P` is the order's id (the escrow's
/// `ActionHash` in the zome), `K` the maker's identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrderView<P, K> {
    pub id: P,
    pub maker: K,
    /// The maker's side: `Sell` orders are asks, `Buy` orders are bids.
    pub side: Side,
    pub price_per_lot: u64,
    pub remaining_lots: u64,
    /// When the escrow was opened (µs). Establishes time priority.
    pub opened_at: i64,
    pub expires_at: i64,
    /// Released by the maker: no further fills possible.
    pub closed: bool,
}

impl<P, K> OrderView<P, K> {
    /// Fillable now: not closed, not expired, and with lots left.
    pub fn is_live(&self, now: i64) -> bool {
        !self.closed && self.remaining_lots > 0 && now < self.expires_at
    }

    /// The order's terms for the remaining quantity, so cost maths reuses
    /// [`OrderTerms`] instead of repeating it.
    fn remaining_terms(&self) -> OrderTerms {
        OrderTerms {
            side: self.side,
            price_per_lot: self.price_per_lot,
            lots: self.remaining_lots,
            expires_at: self.expires_at,
        }
    }
}

/// All orders at one price on one side, merged.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PriceLevel {
    pub price_per_lot: u64,
    pub lots: u64,
    pub orders: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BookView {
    /// Sell orders, best (lowest) price first.
    pub asks: Vec<PriceLevel>,
    /// Buy orders, best (highest) price first.
    pub bids: Vec<PriceLevel>,
    /// Best ask minus best bid, in UNIT-B minor units per lot. `None` if
    /// either side is empty. Negative means the book is crossed; it is never
    /// clamped to zero.
    pub spread: Option<i64>,
}

/// Orders that can be filled now.
pub fn live<P, K>(orders: &[OrderView<P, K>], now: i64) -> impl Iterator<Item = &OrderView<P, K>> {
    orders.iter().filter(move |o| o.is_live(now))
}

/// Book priority within one side: best price first, then oldest, then id.
fn priority<P: Ord, K>(a: &OrderView<P, K>, b: &OrderView<P, K>) -> Ordering {
    let by_price = match a.side {
        Side::Sell => a.price_per_lot.cmp(&b.price_per_lot),
        Side::Buy => b.price_per_lot.cmp(&a.price_per_lot),
    };
    by_price.then_with(|| (a.opened_at, &a.id).cmp(&(b.opened_at, &b.id)))
}

/// Live orders on one side in priority order.
fn side_in_priority<P: Ord, K>(orders: &[OrderView<P, K>], side: Side, now: i64) -> Vec<&OrderView<P, K>> {
    let mut side_orders: Vec<_> = live(orders, now).filter(|o| o.side == side).collect();
    side_orders.sort_by(|a, b| priority(a, b));
    side_orders
}

fn levels<P: Ord, K>(orders: &[OrderView<P, K>], side: Side, now: i64) -> Vec<PriceLevel> {
    let mut levels: Vec<PriceLevel> = Vec::new();
    for order in side_in_priority(orders, side, now) {
        match levels.last_mut() {
            Some(level) if level.price_per_lot == order.price_per_lot => {
                // Lots are bounded by the mint cap, far below u64::MAX.
                level.lots = level.lots.saturating_add(order.remaining_lots);
                level.orders += 1;
            }
            _ => levels.push(PriceLevel {
                price_per_lot: order.price_per_lot,
                lots: order.remaining_lots,
                orders: 1,
            }),
        }
    }
    levels
}

/// Aggregate live orders into price levels.
pub fn aggregate<P: Ord, K>(orders: &[OrderView<P, K>], now: i64) -> BookView {
    let asks = levels(orders, Side::Sell, now);
    let bids = levels(orders, Side::Buy, now);
    let spread = match (asks.first(), bids.first()) {
        (Some(ask), Some(bid)) => {
            // Both are non-negative once converted, so the subtraction cannot overflow.
            match (i64::try_from(ask.price_per_lot), i64::try_from(bid.price_per_lot)) {
                (Ok(ask), Ok(bid)) => Some(ask - bid),
                _ => None,
            }
        }
        _ => None,
    };
    BookView { asks, bids, spread }
}

/// The live orders at one price level, in time priority.
pub fn orders_at_level<P: Ord + Clone, K: Clone>(
    orders: &[OrderView<P, K>],
    side: Side,
    price_per_lot: u64,
    now: i64,
) -> Vec<OrderView<P, K>> {
    side_in_priority(orders, side, now)
        .into_iter()
        .filter(|o| o.price_per_lot == price_per_lot)
        .cloned()
        .collect()
}

/// One order a taker should park against, and exactly what to park.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedFill<P> {
    pub order: P,
    pub price_per_lot: u64,
    pub lots: u64,
    /// What the taker parks: `lots × taker_units_per_lot` of the taker asset.
    pub cost: Amounts,
    /// What the taker receives if the fill settles in full.
    pub receives: Amounts,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TakePlan<P> {
    /// Orders to park against, in the order they were chosen.
    pub fills: Vec<PlannedFill<P>>,
    pub filled: u64,
    /// Requested lots the book could not supply within the limit price.
    pub shortfall: u64,
    pub total_cost: Amounts,
    pub total_receives: Amounts,
}

/// Plan a take of `lots` against the book, best price then oldest first.
///
/// `take` is the taker's direction: `Buy` consumes asks (maker `Sell`
/// orders), `Sell` consumes bids. With a `limit_price`, a buy takes no ask
/// above it and a sell takes no bid below it. The taker's own orders are
/// skipped: a self-trade is legal in the ledger but pointless here.
///
/// The plan is a snapshot. Other takers can fill the same orders first; the
/// maker's run then refunds whatever it cannot fill.
pub fn plan_take<P: Ord + Clone, K: PartialEq>(
    orders: &[OrderView<P, K>],
    taker: &K,
    take: Side,
    lots: u64,
    limit_price: Option<u64>,
    now: i64,
) -> Result<TakePlan<P>, CoreError> {
    let maker_side = match take {
        Side::Buy => Side::Sell,
        Side::Sell => Side::Buy,
    };
    let within_limit = |price: u64| match (take, limit_price) {
        (_, None) => true,
        (Side::Buy, Some(limit)) => price <= limit,
        (Side::Sell, Some(limit)) => price >= limit,
    };

    let mut plan = TakePlan {
        fills: Vec::new(),
        filled: 0,
        shortfall: lots,
        total_cost: Amounts::ZERO,
        total_receives: Amounts::ZERO,
    };
    for order in side_in_priority(orders, maker_side, now) {
        if plan.shortfall == 0 || !within_limit(order.price_per_lot) {
            // Priority order means every later order is also past the limit.
            break;
        }
        if &order.maker == taker {
            continue;
        }
        let fill = plan.shortfall.min(order.remaining_lots);
        let terms = order.remaining_terms();
        let cost = Amounts::of(
            terms.taker_asset(),
            fill.checked_mul(terms.taker_units_per_lot()).ok_or(CoreError::Overflow)?,
        );
        let receives = Amounts::of(
            terms.maker_asset(),
            fill.checked_mul(terms.maker_units_per_lot()).ok_or(CoreError::Overflow)?,
        );
        plan.total_cost = plan.total_cost.checked_add(cost).ok_or(CoreError::Overflow)?;
        plan.total_receives = plan.total_receives.checked_add(receives).ok_or(CoreError::Overflow)?;
        plan.filled += fill;
        plan.shortfall -= fill;
        plan.fills.push(PlannedFill {
            order: order.id.clone(),
            price_per_lot: order.price_per_lot,
            lots: fill,
            cost,
            receives,
        });
    }
    Ok(plan)
}

#[cfg(test)]
mod tests;
