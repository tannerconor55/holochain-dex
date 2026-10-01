use super::*;
use crate::{Amounts, MarketDef, OrderTerms, RunMode, Side};

const H: i64 = 3_600_000_000;

#[test]
fn days_floor_at_utc_midnight_also_before_the_epoch() {
    assert_eq!(utc_day(0), 0);
    assert_eq!(utc_day(DAY_US - 1), 0);
    assert_eq!(utc_day(DAY_US), 1);
    assert_eq!(utc_day(-1), -1);
    assert_eq!(utc_day(-DAY_US), -1);
    assert_eq!(utc_day(-DAY_US - 1), -2);
    // 2026-10-01T00:00:00Z is day 20,727.
    assert_eq!(utc_day(1_790_812_800_000_000), 20_727);
}

#[test]
fn live_listings_cover_the_lifetime_back_from_now() {
    let lifetime = 7 * DAY_US;
    let now = 20_727 * DAY_US + 5 * H; // 05:00 on day 20,727
    let days = live_listing_days(now, lifetime);
    assert_eq!(days, 20_720..=20_727, "eight days: the partial first one included");
    // A listing created just inside the window lands in the first day read.
    assert!(days.contains(&utc_day(now - lifetime)));
    // At exactly midnight the window is still eight days.
    assert_eq!(live_listing_days(20_727 * DAY_US, lifetime).count(), 8);
}

#[test]
fn ranges_are_half_open() {
    assert_eq!(days_between(0, DAY_US), 0..=0, "`to` exclusive: midnight is not the next day");
    assert_eq!(days_between(0, DAY_US + 1), 0..=1);
    assert_eq!(days_between(DAY_US - 1, DAY_US + 1), 0..=1);
    assert_eq!(days_between(5, 5).count(), 0);
    assert_eq!(days_between(5, 4).count(), 0);
}

#[test]
fn lifetime_is_bounded_and_expiry_follows_creation() {
    let l = 7 * DAY_US;
    assert!(within_lifetime(0, l, l), "exactly the lifetime");
    assert!(!within_lifetime(0, l + 1, l));
    assert!(!within_lifetime(10, 10, l), "expiring when created");
    assert!(!within_lifetime(10, 5, l));
    assert!(!within_lifetime(i64::MIN, i64::MAX, l), "no overflow");
}

fn sell(lots: u64) -> OrderTerms {
    OrderTerms { side: Side::Sell, price_per_lot: 120, lots, expires_at: i64::MAX }
}

#[test]
fn a_run_sells_what_its_lock_fell_by() {
    let m = MarketDef::default_pair();
    let t = sell(100);
    let full = Amounts::new(10_000, 0);
    assert_eq!(run_sold_lots(&t, &m, &full, &Amounts::new(6_000, 0), RunMode::Fill), Ok(40));
    assert_eq!(run_sold_lots(&t, &m, &full, &full, RunMode::Fill), Ok(0), "a fill that only refunds");
    assert_eq!(run_sold_lots(&t, &m, &full, &Amounts::ZERO, RunMode::Release), Ok(0), "a release sells nothing");
    assert_eq!(
        run_sold_lots(&t, &m, &Amounts::new(6_000, 0), &full, RunMode::Fill),
        Err(CoreError::InconsistentLock),
        "a lock cannot grow"
    );
    let buy = OrderTerms { side: Side::Buy, ..sell(10) };
    assert_eq!(run_sold_lots(&buy, &m, &Amounts::new(0, 1_200), &Amounts::new(0, 720), RunMode::Fill), Ok(4));
}

#[test]
fn trade_tags_round_trip_and_refuse_anything_else() {
    let tag = TradeTag { price_per_lot: 121, lots: 40, maker_side: Side::Buy, run_ts: -5, escrow: vec![0x84; 39] };
    let bytes = tag.encode();
    assert_eq!(bytes.len(), 26 + 39);
    assert_eq!(TradeTag::decode(&bytes), Some(tag.clone()));
    assert_eq!(TradeTag::decode(&bytes[..26]), None, "no escrow");
    let mut wrong_version = bytes.clone();
    wrong_version[0] = 2;
    assert_eq!(TradeTag::decode(&wrong_version), None);
    let mut wrong_side = bytes;
    wrong_side[17] = 7;
    assert_eq!(TradeTag::decode(&wrong_side), None);
    assert_eq!(TradeTag::decode(&[]), None);
}
