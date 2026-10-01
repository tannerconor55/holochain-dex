//! # Day buckets and the trade index tag
//!
//! Listings and trades hang off per-market, per-day anchors so a read only
//! fetches the days that can matter (design `docs/design/read-performance.md`).
//! A day is a UTC day number: `floor(µs since the epoch / 86,400 s)`.
//!
//! A listing's day is its escrow's creation day, and an order lives at most
//! `max_order_lifetime` (DNA property), so a listing from before
//! `day(now − lifetime)` is certainly expired: the book reads
//! [`live_listing_days`]. A trade's day is its run's day.

use crate::{Amounts, CoreError, MarketDef, OrderTerms, RunMode, Side};
use core::ops::RangeInclusive;

pub const DAY_US: i64 = 86_400 * 1_000_000;

/// How far back trade reads look for history (and for a last price).
pub const TRADE_LOOKBACK_DAYS: i64 = 30;

/// The UTC day holding `ts_us` (floor, also before the epoch).
pub fn utc_day(ts_us: i64) -> i64 {
    ts_us.div_euclid(DAY_US)
}

/// The days a listing that could still be live was created on, oldest
/// first: a listing created before `now − max_lifetime` has expired.
pub fn live_listing_days(now_us: i64, max_lifetime_us: i64) -> RangeInclusive<i64> {
    utc_day(now_us.saturating_sub(max_lifetime_us))..=utc_day(now_us)
}

/// The days holding `[from_us, to_us)`, oldest first; empty if `to <= from`.
pub fn days_between(from_us: i64, to_us: i64) -> RangeInclusive<i64> {
    if to_us <= from_us {
        #[allow(clippy::reversed_empty_ranges)]
        return 1..=0;
    }
    utc_day(from_us)..=utc_day(to_us - 1)
}

/// Whether an order created at `created_us` and expiring at `expires_at` is
/// within the maximum lifetime (and expires after it is created).
pub fn within_lifetime(created_us: i64, expires_at: i64, max_lifetime_us: i64) -> bool {
    expires_at > created_us && expires_at.saturating_sub(created_us) <= max_lifetime_us
}

/// Lots a run sold: how far its lock fell, in lots, for a `Fill`; zero for a
/// `Release` (it refunds and returns the lock). A lock that grew is an error.
pub fn run_sold_lots(
    terms: &OrderTerms,
    market: &MarketDef,
    prev_locked: &Amounts,
    locked: &Amounts,
    mode: RunMode,
) -> Result<u64, CoreError> {
    let before = terms.remaining_lots(prev_locked, market)?;
    let after = terms.remaining_lots(locked, market)?;
    match mode {
        RunMode::Fill => before.checked_sub(after).ok_or(CoreError::InconsistentLock),
        RunMode::Release => Ok(0),
    }
}

/// What a trade index link's tag says, checked by validation against the run.
/// `escrow` is the escrow action hash's raw bytes (opaque here).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TradeTag {
    pub price_per_lot: u64,
    pub lots: u64,
    pub maker_side: Side,
    /// The run's action timestamp, µs.
    pub run_ts: i64,
    pub escrow: Vec<u8>,
}

const TRADE_TAG_VERSION: u8 = 1;
const TRADE_TAG_FIXED: usize = 1 + 8 + 8 + 1 + 8;

impl TradeTag {
    /// `version u8 | price u64 BE | lots u64 BE | side u8 | run_ts i64 BE | escrow`.
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(TRADE_TAG_FIXED + self.escrow.len());
        out.push(TRADE_TAG_VERSION);
        out.extend_from_slice(&self.price_per_lot.to_be_bytes());
        out.extend_from_slice(&self.lots.to_be_bytes());
        out.push(match self.maker_side {
            Side::Sell => 0,
            Side::Buy => 1,
        });
        out.extend_from_slice(&self.run_ts.to_be_bytes());
        out.extend_from_slice(&self.escrow);
        out
    }

    /// `None` for anything `encode` cannot have produced.
    pub fn decode(bytes: &[u8]) -> Option<TradeTag> {
        if bytes.len() <= TRADE_TAG_FIXED || bytes[0] != TRADE_TAG_VERSION {
            return None;
        }
        let u64_at = |i: usize| bytes.get(i..i + 8).and_then(|b| b.try_into().ok()).map(u64::from_be_bytes);
        let maker_side = match bytes[17] {
            0 => Side::Sell,
            1 => Side::Buy,
            _ => return None,
        };
        Some(TradeTag {
            price_per_lot: u64_at(1)?,
            lots: u64_at(9)?,
            maker_side,
            run_ts: u64_at(18)? as i64,
            escrow: bytes[TRADE_TAG_FIXED..].to_vec(),
        })
    }
}

#[cfg(test)]
mod tests;
