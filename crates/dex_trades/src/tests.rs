use super::*;
use dex_core::{Amounts, MarketDef, OrderTerms, RunMode, Side};

const MIN: i64 = 60_000_000;

fn a_hf() -> MarketDef {
    MarketDef::default_pair()
}

fn sell(lots: u64, price: u64) -> OrderTerms {
    OrderTerms { side: Side::Sell, price_per_lot: price, lots, expires_at: i64::MAX }
}

fn run(id: u32, t: i64, mode: RunMode, locked_a: u64) -> RunRecord<u32> {
    RunRecord { run: id, timestamp: t, mode, locked: Amounts::new(locked_a, 0) }
}

/// A trade at `t` of `lots` at `price`, run id `id`.
fn trade(id: u32, t: i64, price: u64, lots: u64) -> Trade<u32> {
    Trade {
        market: a_hf().id(),
        escrow: 0,
        run: id,
        timestamp: t,
        price_per_lot: price,
        lots,
        maker_side: Side::Sell,
        quote: lots * price,
    }
}

#[test]
fn fill_runs_become_trades_and_a_release_does_not() {
    // Sell 100 lots at 1.20: fill 40, fill nothing, fill 25, then release 35.
    let runs = [
        run(1, 10, RunMode::Fill, 6_000),
        run(2, 20, RunMode::Fill, 6_000),
        run(3, 30, RunMode::Fill, 3_500),
        run(4, 40, RunMode::Release, 0),
    ];
    let trades = trades_of_order(&9u32, &a_hf(), &sell(100, 120), &runs).unwrap();
    let got: Vec<(u32, i64, u64, u64)> = trades.iter().map(|t| (t.run, t.timestamp, t.lots, t.quote)).collect();
    assert_eq!(got, vec![(1, 10, 40, 4_800), (3, 30, 25, 3_000)]);
    assert!(trades.iter().all(|t| t.escrow == 9 && t.price_per_lot == 120 && t.maker_side == Side::Sell));
    assert!(trades.iter().all(|t| t.market == a_hf().id()));
}

#[test]
fn a_buy_order_counts_lots_from_its_quote_lock() {
    let terms = OrderTerms { side: Side::Buy, ..sell(10, 120) };
    // Buy 10 lots at 1.20 locks 12.00 HF; a fill of 4 leaves 7.20 HF.
    let runs = [RunRecord { run: 1u32, timestamp: 5, mode: RunMode::Fill, locked: Amounts::new(0, 720) }];
    let trades = trades_of_order(&0u32, &a_hf(), &terms, &runs).unwrap();
    assert_eq!((trades[0].lots, trades[0].maker_side, trades[0].quote), (4, Side::Buy, 480));
}

#[test]
fn a_lock_that_grows_or_is_not_whole_lots_is_refused() {
    let runs = [run(1, 1, RunMode::Fill, 6_000), run(2, 2, RunMode::Fill, 7_000)];
    assert_eq!(trades_of_order(&0u32, &a_hf(), &sell(100, 120), &runs), Err(TradesError::LockGrew));
    let ragged = [run(1, 1, RunMode::Fill, 6_050)];
    assert!(matches!(trades_of_order(&0u32, &a_hf(), &sell(100, 120), &ragged), Err(TradesError::Core(_))));
}

#[test]
fn newest_first_breaks_ties_by_run_id() {
    let mut ts = vec![trade(1, 10, 120, 1), trade(3, 20, 121, 1), trade(2, 20, 119, 1)];
    sort_newest_first(&mut ts);
    assert_eq!(ts.iter().map(|t| t.run).collect::<Vec<_>>(), vec![3, 2, 1]);
}

#[test]
fn stats_cover_the_last_24_hours_and_the_last_price_ever() {
    let now = 10 * DAY_US;
    let trades = [
        trade(1, now - DAY_US, 100, 5),     // exactly 24 h ago: outside
        trade(2, now - DAY_US + 1, 118, 10), // just inside: the open
        trade(3, now - 60 * MIN, 125, 4),
        trade(5, now - MIN, 121, 1),
        trade(4, now - MIN, 110, 2), // same µs as run 5, lower id: not the close
        trade(6, now + 1, 999, 1),   // after now: ignored
    ];
    let s = market_stats(&trades, now).unwrap();
    assert_eq!(s.last_price, Some(121));
    assert_eq!(s.last_trade_at, Some(now - MIN));
    assert_eq!((s.open_24h, s.change_24h), (Some(118), Some(3)));
    assert_eq!((s.high_24h, s.low_24h), (Some(125), Some(110)));
    assert_eq!((s.volume_lots_24h, s.trades_24h), (17, 4));
    assert_eq!(s.volume_quote_24h, 10 * 118 + 4 * 125 + 121 + 2 * 110);
}

#[test]
fn stats_without_recent_trades_keep_the_last_price() {
    let now = 10 * DAY_US;
    let s = market_stats(&[trade(1, now - 3 * DAY_US, 130, 2)], now).unwrap();
    assert_eq!(s.last_price, Some(130));
    assert_eq!((s.open_24h, s.change_24h, s.high_24h, s.volume_lots_24h), (None, None, None, 0));
    assert_eq!(market_stats::<u32>(&[], now).unwrap(), MarketStats::default());
    // A falling day: change is negative.
    let s = market_stats(&[trade(1, now - 2 * MIN, 130, 1), trade(2, now - MIN, 125, 1)], now).unwrap();
    assert_eq!(s.change_24h, Some(-5));
}

#[test]
fn candles_bucket_by_interval_with_open_high_low_close() {
    let base = 1_000 * CandleInterval::M5.micros();
    let trades = [
        trade(2, base + MIN, 122, 3),
        trade(1, base, 120, 1),
        trade(3, base + 4 * MIN, 118, 2),
        trade(4, base + 5 * MIN, 130, 5), // next bucket
        trade(5, base + 20 * MIN, 125, 1), // skips two empty buckets
    ];
    let cs = candles(&trades, CandleInterval::M5, base, base + 30 * MIN).unwrap();
    let starts: Vec<i64> = cs.iter().map(|c| c.start).collect();
    assert_eq!(starts, vec![base, base + 5 * MIN, base + 20 * MIN], "empty intervals have no candle");
    let c = &cs[0];
    assert_eq!((c.open, c.high, c.low, c.close), (120, 122, 118, 118));
    assert_eq!((c.volume_lots, c.volume_quote, c.trades), (6, 120 + 366 + 236, 3));
    assert_eq!((cs[1].open, cs[1].close, cs[1].trades), (130, 130, 1));
}

#[test]
fn candle_range_is_half_open_and_bounded() {
    let t0 = 50 * CandleInterval::H1.micros();
    let trades = [trade(1, t0, 100, 1), trade(2, t0 + CandleInterval::H1.micros(), 101, 1)];
    let cs = candles(&trades, CandleInterval::H1, t0, t0 + CandleInterval::H1.micros()).unwrap();
    assert_eq!(cs.len(), 1, "`to` is exclusive");
    assert_eq!(candles(&trades, CandleInterval::H1, t0, t0), Err(TradesError::EmptyRange));
    let too_wide = MAX_CANDLES * CandleInterval::M1.micros();
    assert_eq!(candles(&trades, CandleInterval::M1, 0, too_wide), Err(TradesError::TooManyCandles));
    assert!(candles(&trades, CandleInterval::M1, 0, too_wide - 1).is_ok());
}

#[test]
fn buckets_align_to_the_epoch_before_it_too() {
    let w = CandleInterval::M15.micros();
    assert_eq!(bucket_start(0, CandleInterval::M15), 0);
    assert_eq!(bucket_start(w - 1, CandleInterval::M15), 0);
    assert_eq!(bucket_start(-1, CandleInterval::M15), -w, "floor, not truncation");
    assert_eq!(CandleInterval::D1.micros(), DAY_US);
}

/// Over generated run sequences: the traded lots equal the order's filled
/// lots, and the candles' volume equals the trades' volume.
#[test]
fn volumes_add_up_across_generated_orders() {
    let mut seed: u64 = 7;
    let mut next = move |m: u64| {
        seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        (seed >> 33) % m
    };
    for case in 0..500u32 {
        let lots = 1 + next(200);
        let terms = sell(lots, 1 + next(500));
        let mut remaining = lots;
        let mut t = next(1_000) as i64 * MIN;
        let mut runs = Vec::new();
        for i in 0..next(12) as u32 {
            t += next(30) as i64 * MIN;
            let released = next(8) == 0;
            remaining = if released { 0 } else { remaining - next(remaining + 1) };
            let mode = if released { RunMode::Release } else { RunMode::Fill };
            runs.push(run(case * 100 + i, t, mode, remaining * 100));
            if released {
                break;
            }
        }
        let trades = trades_of_order(&case, &a_hf(), &terms, &runs).unwrap();
        let filled_before_release: u64 = {
            let mut left = lots;
            let mut filled = 0;
            for r in &runs {
                let after = r.locked.get(dex_core::UNIT_A) / 100;
                if r.mode == RunMode::Fill {
                    filled += left - after;
                }
                left = after;
            }
            filled
        };
        assert_eq!(trades.iter().map(|t| t.lots).sum::<u64>(), filled_before_release, "case {case}");
        let cs = candles(&trades, CandleInterval::H1, 0, 900 * CandleInterval::H1.micros()).unwrap();
        assert_eq!(cs.iter().map(|c| c.volume_lots).sum::<u64>(), filled_before_release, "case {case}");
        assert_eq!(cs.iter().map(|c| c.trades).sum::<u64>(), trades.len() as u64);
    }
}
