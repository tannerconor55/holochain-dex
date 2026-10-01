//! # dex_trades — trades, market stats and candles
//!
//! Derived from settlement runs, nothing stored. Its own crate rather than a
//! `dex_core` module: the integrity zomes link `dex_core`, so any change there
//! changes the DNA hash, and validation never needs trade history. A `Fill` run's lock shrinks
//! by exactly the lots it filled, all at the order's own price, so one run
//! is one trade; a `Release` run fills nothing (it refunds and returns the
//! lock). Integer maths only: prices are quote minor units per lot, volumes
//! are lots and quote minor units.

use dex_core::{Amounts, CoreError, MarketDef, MarketId, OrderTerms, RunMode, Side};
use serde::{Deserialize, Serialize};

/// One run of an order, as the ledger records it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunRecord<H> {
    pub run: H,
    /// The run's action timestamp, µs since the epoch.
    pub timestamp: i64,
    pub mode: RunMode,
    /// The lock left after the run.
    pub locked: Amounts,
}

/// Lots changing hands in one run at the order's price.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Trade<H> {
    pub market: MarketId,
    pub escrow: H,
    pub run: H,
    /// µs since the epoch: the run's timestamp, which the maker asserts
    /// (as Unyt's `executed_timestamp` is the executor's).
    pub timestamp: i64,
    pub price_per_lot: u64,
    pub lots: u64,
    /// The resting order's side; the taker traded the other way.
    pub maker_side: Side,
    /// `lots × price_per_lot`, quote minor units.
    pub quote: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TradesError {
    /// Includes a run's lock growing or not being whole lots
    /// (`CoreError::InconsistentLock`).
    Core(CoreError),
    Overflow,
    /// `to` is not after `from`.
    EmptyRange,
    /// The range holds more than `MAX_CANDLES` intervals.
    TooManyCandles,
}

impl From<CoreError> for TradesError {
    fn from(e: CoreError) -> Self {
        match e {
            CoreError::Overflow => TradesError::Overflow,
            other => TradesError::Core(other),
        }
    }
}

impl core::fmt::Display for TradesError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            TradesError::Core(e) => write!(f, "{e}"),
            TradesError::Overflow => write!(f, "arithmetic overflow"),
            TradesError::EmptyRange => write!(f, "the range must end after it starts"),
            TradesError::TooManyCandles => write!(f, "at most {MAX_CANDLES} candles per request"),
        }
    }
}

impl std::error::Error for TradesError {}

/// The trades of one order, oldest first, from its runs oldest first.
pub fn trades_of_order<H: Clone>(
    escrow: &H,
    market: &MarketDef,
    terms: &OrderTerms,
    runs: &[RunRecord<H>],
) -> Result<Vec<Trade<H>>, TradesError> {
    let mut prev_locked = terms.initial_lock(market)?;
    let mut out = Vec::new();
    for r in runs {
        // The same rule the trade index's validation applies.
        let lots = dex_core::buckets::run_sold_lots(terms, market, &prev_locked, &r.locked, r.mode)?;
        {
            if lots > 0 {
                out.push(Trade {
                    market: market.id(),
                    escrow: escrow.clone(),
                    run: r.run.clone(),
                    timestamp: r.timestamp,
                    price_per_lot: terms.price_per_lot,
                    lots,
                    maker_side: terms.side,
                    quote: lots.checked_mul(terms.price_per_lot).ok_or(TradesError::Overflow)?,
                });
            }
        }
        prev_locked = r.locked.clone();
    }
    Ok(out)
}

/// Newest first; ties (one microsecond) by run id, descending.
pub fn sort_newest_first<H: Ord>(trades: &mut [Trade<H>]) {
    trades.sort_by(|a, b| (b.timestamp, &b.run).cmp(&(a.timestamp, &a.run)));
}

pub const DAY_US: i64 = 24 * 3_600 * 1_000_000;

/// Last price and the 24 hours up to `now`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct MarketStats {
    /// The latest trade's price, however old; `None` before any trade.
    pub last_price: Option<u64>,
    pub last_trade_at: Option<i64>,
    /// The first trade in the window's price, and `last − open`, quote
    /// minor units per lot.
    pub open_24h: Option<u64>,
    pub change_24h: Option<i64>,
    pub high_24h: Option<u64>,
    pub low_24h: Option<u64>,
    pub volume_lots_24h: u64,
    pub volume_quote_24h: u64,
    pub trades_24h: u64,
}

/// Stats over `trades` (any order): the window is `(now − 24 h, now]`.
pub fn market_stats<H: Ord>(trades: &[Trade<H>], now: i64) -> Result<MarketStats, TradesError> {
    let order = |a: &&Trade<H>, b: &&Trade<H>| (a.timestamp, &a.run).cmp(&(b.timestamp, &b.run));
    let mut stats = MarketStats::default();
    if let Some(last) = trades.iter().filter(|t| t.timestamp <= now).max_by(order) {
        stats.last_price = Some(last.price_per_lot);
        stats.last_trade_at = Some(last.timestamp);
    }
    let start = now.checked_sub(DAY_US).ok_or(TradesError::Overflow)?;
    let window: Vec<&Trade<H>> = trades.iter().filter(|t| t.timestamp > start && t.timestamp <= now).collect();
    if let (Some(open), Some(close)) = (window.iter().copied().min_by(order), window.iter().copied().max_by(order)) {
        stats.open_24h = Some(open.price_per_lot);
        let (o, c) = (i64::try_from(open.price_per_lot), i64::try_from(close.price_per_lot));
        stats.change_24h = Some(match (o, c) {
            (Ok(o), Ok(c)) => c.checked_sub(o).ok_or(TradesError::Overflow)?,
            _ => return Err(TradesError::Overflow),
        });
    }
    stats.high_24h = window.iter().map(|t| t.price_per_lot).max();
    stats.low_24h = window.iter().map(|t| t.price_per_lot).min();
    for t in window.iter() {
        stats.volume_lots_24h = stats.volume_lots_24h.checked_add(t.lots).ok_or(TradesError::Overflow)?;
        stats.volume_quote_24h = stats.volume_quote_24h.checked_add(t.quote).ok_or(TradesError::Overflow)?;
    }
    stats.trades_24h = window.len() as u64;
    Ok(stats)
}

/// Candle widths. Buckets are aligned to the Unix epoch in UTC.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CandleInterval {
    M1,
    M5,
    M15,
    H1,
    H4,
    D1,
}

impl CandleInterval {
    pub const fn micros(self) -> i64 {
        const MIN: i64 = 60_000_000;
        match self {
            CandleInterval::M1 => MIN,
            CandleInterval::M5 => 5 * MIN,
            CandleInterval::M15 => 15 * MIN,
            CandleInterval::H1 => 60 * MIN,
            CandleInterval::H4 => 240 * MIN,
            CandleInterval::D1 => 1_440 * MIN,
        }
    }
}

/// Most candles one request may span (one response stays small).
pub const MAX_CANDLES: i64 = 1_000;

/// Open, high, low, close over one interval. Only intervals holding a trade
/// get a candle: a chart carries the last close across gaps.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Candle {
    /// Start of the interval, µs since the epoch.
    pub start: i64,
    pub open: u64,
    pub high: u64,
    pub low: u64,
    pub close: u64,
    pub volume_lots: u64,
    pub volume_quote: u64,
    pub trades: u64,
}

/// The start of the interval holding `t`.
pub fn bucket_start(t: i64, interval: CandleInterval) -> i64 {
    let w = interval.micros();
    t.div_euclid(w) * w
}

/// Candles for trades with `from <= timestamp < to`, oldest first. Open and
/// close are the first and last trade by (timestamp, run id).
pub fn candles<H: Ord>(
    trades: &[Trade<H>],
    interval: CandleInterval,
    from: i64,
    to: i64,
) -> Result<Vec<Candle>, TradesError> {
    if to <= from {
        return Err(TradesError::EmptyRange);
    }
    let span = to.checked_sub(from).ok_or(TradesError::Overflow)?;
    if span / interval.micros() >= MAX_CANDLES {
        return Err(TradesError::TooManyCandles);
    }
    let mut ordered: Vec<&Trade<H>> = trades.iter().filter(|t| t.timestamp >= from && t.timestamp < to).collect();
    ordered.sort_by(|a, b| (a.timestamp, &a.run).cmp(&(b.timestamp, &b.run)));
    let mut out: Vec<Candle> = Vec::new();
    for t in ordered {
        let start = bucket_start(t.timestamp, interval);
        match out.last_mut() {
            Some(c) if c.start == start => {
                c.high = c.high.max(t.price_per_lot);
                c.low = c.low.min(t.price_per_lot);
                c.close = t.price_per_lot;
                c.volume_lots = c.volume_lots.checked_add(t.lots).ok_or(TradesError::Overflow)?;
                c.volume_quote = c.volume_quote.checked_add(t.quote).ok_or(TradesError::Overflow)?;
                c.trades += 1;
            }
            _ => out.push(Candle {
                start,
                open: t.price_per_lot,
                high: t.price_per_lot,
                low: t.price_per_lot,
                close: t.price_per_lot,
                volume_lots: t.lots,
                volume_quote: t.quote,
                trades: 1,
            }),
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests;
