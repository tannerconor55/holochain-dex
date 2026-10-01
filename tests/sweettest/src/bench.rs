//! Read-cost benchmark (milestone 4): how book and price reads scale with a
//! market's history. Ignored by default; run with
//!
//! ```text
//! DEX_BENCH_SIZES=10,100,1000 nix develop -c cargo test \
//!   --manifest-path tests/sweettest/Cargo.toml bench:: -- --ignored --nocapture
//! ```
//!
//! For each size N, one maker lists N orders in the default market: 10 stay
//! live and the rest are split evenly between cancelled, filled (one trade
//! each) and expired. A second agent, on another conductor, then times each
//! read (median of 3 after one warm-up). A mature market looks like this:
//! a few live orders on top of a long closed history.

use super::*;
use dex_api::{Candle, CandleInterval, CandlesRequest, MarketStats, RecentTradesRequest, Trade};
use std::time::{Duration, Instant};

const LIVE: usize = 10;

struct Row {
    n: usize,
    listed: usize,
    trades: usize,
    book: Duration,
    stats: Duration,
    candles: Duration,
    recent: Duration,
}

async fn median<F, Fut, T>(mut f: F) -> Duration
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = T>,
{
    let _ = f().await; // warm-up
    let mut times = Vec::new();
    for _ in 0..3 {
        let t = Instant::now();
        let _ = f().await;
        times.push(t.elapsed());
    }
    times.sort();
    times[1]
}

async fn measure(n: usize) -> Row {
    let env = TestEnv::new(2).await;
    let history = n.saturating_sub(LIVE);
    let (cancelled, filled) = (history / 3, history / 3);
    let expired = history - cancelled - filled;
    let live = n - history;
    let _: ActionHash = env.call(ALICE, "mint", Amounts::new(100 * n as u64, 0)).await;
    let _: ActionHash = env.call(BOB, "mint", Amounts::new(0, 120 * n as u64)).await;

    let setup = Instant::now();
    // Expired: listed first with a short life, so they lapse during setup.
    let mut last_expiry = 0;
    for _ in 0..expired {
        let expires_at = Timestamp::now().as_micros() + 20_000_000;
        last_expiry = expires_at;
        let _: ActionHash = env.dex(ALICE, "place_order", place(OrderTerms { expires_at, ..sell(1, 120) })).await;
    }
    let mut to_cancel = Vec::new();
    for _ in 0..cancelled {
        to_cancel.push(env.dex::<_, ActionHash>(ALICE, "place_order", place(sell(1, 130))).await);
    }
    let mut to_fill = Vec::new();
    for _ in 0..filled {
        to_fill.push(env.dex::<_, ActionHash>(ALICE, "place_order", place(sell(1, 125))).await);
    }
    for _ in 0..live {
        let _: ActionHash = env.dex(ALICE, "place_order", place(sell(1, 140))).await;
    }
    for escrow in &to_cancel {
        let _: Vec<RunReport> = env.dex(ALICE, "cancel_order", escrow.clone()).await;
    }
    env.sync().await;
    for escrow in &to_fill {
        let _: ActionHash = env.call(BOB, "park", park_request(escrow, 125, 1)).await;
    }
    env.sync().await;
    for escrow in &to_fill {
        let _: Vec<RunReport> = env.dex(ALICE, "settle_order", escrow.clone()).await;
    }
    while Timestamp::now().as_micros() <= last_expiry {
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    env.sync().await;
    eprintln!("bench: N={n} set up in {:.0} s", setup.elapsed().as_secs_f64());

    // Sanity: the reads see what was built.
    let book = env.book(BOB).await;
    assert_eq!(book.asks.iter().map(|l| l.orders).sum::<usize>(), live, "only live orders are in the book");
    let trades: Vec<Trade> = env.dex(BOB, "get_recent_trades", RecentTradesRequest { market: None, limit: 200 }).await;
    assert_eq!(trades.len(), filled.min(200));

    let now = Timestamp::now().as_micros();
    let candles_req = || CandlesRequest {
        market: None,
        interval: CandleInterval::M15,
        from: now - 86_400_000_000,
        to: now + 900_000_000,
    };
    let row = Row {
        n,
        listed: n,
        trades: filled,
        book: median(|| env.dex::<_, BookView>(BOB, "get_order_book", None::<dex_core::MarketId>)).await,
        stats: median(|| env.dex::<_, MarketStats>(BOB, "get_market_stats", None::<dex_core::MarketId>)).await,
        candles: median(|| env.dex::<_, Vec<Candle>>(BOB, "get_candles", candles_req())).await,
        recent: median(|| env.dex::<_, Vec<Trade>>(BOB, "get_recent_trades", RecentTradesRequest { market: None, limit: 30 })).await,
    };
    env.assert_supply().await;
    row
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "benchmark: run explicitly (see module docs)"]
async fn read_cost_by_market_history() {
    let sizes: Vec<usize> = std::env::var("DEX_BENCH_SIZES")
        .unwrap_or_else(|_| "10,100,1000".into())
        .split(',')
        .map(|s| s.trim().parse().expect("DEX_BENCH_SIZES is a comma-separated list of sizes"))
        .collect();
    let mut rows = Vec::new();
    for n in sizes {
        let row = measure(n).await;
        eprintln!(
            "bench: N={} book {:?} stats {:?} candles {:?} recent {:?}",
            row.n, row.book, row.stats, row.candles, row.recent
        );
        rows.push(row);
    }
    println!("\n| Orders listed | Trades | Book | Stats | Candles (24 h) | Recent trades |");
    println!("|---:|---:|---:|---:|---:|---:|");
    for r in rows {
        println!(
            "| {} | {} | {} ms | {} ms | {} ms | {} ms |",
            r.listed,
            r.trades,
            r.book.as_millis(),
            r.stats.as_millis(),
            r.candles.as_millis(),
            r.recent.as_millis()
        );
    }
}
