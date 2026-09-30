//! Trade history and price data on real conductors: derived from runs, read
//! by any agent.

use super::*;
use dex_api::{Candle, CandleInterval, CandlesRequest, MarketStats, RecentTradesRequest, Trade};

impl TestEnv {
    async fn recent(&self, agent: usize, limit: u32) -> Vec<Trade> {
        self.dex(agent, "get_recent_trades", RecentTradesRequest { market: None, limit }).await
    }

    async fn stats(&self, agent: usize) -> MarketStats {
        self.dex(agent, "get_market_stats", None::<dex_core::MarketId>).await
    }
}

fn summary(trades: &[Trade]) -> Vec<(ActionHash, u64, u64)> {
    trades.iter().map(|t| (t.escrow.clone(), t.price_per_lot, t.lots)).collect()
}

/// Trades appear once the makers' runs fill, newest first, one per run at
/// the order's price; a cancel adds none. Stats and candles follow.
#[tokio::test(flavor = "multi_thread")]
async fn trades_appear_after_fills_and_stats_follow() {
    let env = TestEnv::new(3).await;
    let _: ActionHash = env.call(ALICE, "mint", Amounts::new(4_000, 0)).await;
    let _: ActionHash = env.call(CAROL, "mint", Amounts::new(3_500, 0)).await;
    let _: ActionHash = env.call(BOB, "mint", Amounts::new(0, 20_000)).await;
    let alice_order: ActionHash = env.dex(ALICE, "place_order", place(sell(40, 120))).await;
    let carol_order: ActionHash = env.dex(CAROL, "place_order", place(sell(35, 121))).await;
    env.sync().await;

    assert!(env.recent(BOB, 10).await.is_empty(), "no trade before any fill");
    assert_eq!(env.stats(BOB).await, MarketStats::default());

    // A 60-lot market buy parks against both; nothing trades until they run.
    let _: dex_api::MarketResult = env
        .dex(BOB, "market_order", dex_api::MarketOrderRequest {
            market: None,
            side: Side::Buy,
            lots: 60,
            max_slippage_bps: Some(200),
            expected: None,
        })
        .await;
    env.sync().await;
    assert!(env.recent(BOB, 10).await.is_empty(), "a park is not a trade");

    let _: Vec<RunReport> = env.dex(ALICE, "run_my_orders", ()).await;
    env.sync().await;
    assert_eq!(summary(&env.recent(BOB, 10).await), vec![(alice_order.clone(), 120, 40)]);
    let _: Vec<RunReport> = env.dex(CAROL, "run_my_orders", ()).await;
    env.sync().await;

    // Another 5 lots from Carol's order, and Carol cancels the rest (no trade).
    let _: TakeResult = env
        .dex(BOB, "take", TakeRequest { market: None, take: Side::Buy, lots: 5, limit_price: None })
        .await;
    env.sync().await;
    let _: Vec<RunReport> = env.dex(CAROL, "run_my_orders", ()).await;
    let _: Vec<RunReport> = env.dex(CAROL, "cancel_order", carol_order.clone()).await;
    env.sync().await;

    let trades = env.recent(ALICE, 10).await;
    assert_eq!(
        summary(&trades),
        vec![(carol_order.clone(), 121, 5), (carol_order.clone(), 121, 20), (alice_order.clone(), 120, 40)],
        "newest first; the cancel is not a trade"
    );
    assert!(trades.iter().all(|t| t.maker_side == Side::Sell && t.quote == t.lots * t.price_per_lot));
    assert!(trades.windows(2).all(|w| w[0].timestamp >= w[1].timestamp));
    assert_eq!(summary(&env.recent(ALICE, 1).await), vec![(carol_order.clone(), 121, 5)], "the limit applies");

    // Every agent derives the same stats.
    let stats = env.stats(CAROL).await;
    assert_eq!(stats, env.stats(BOB).await);
    assert_eq!(stats.last_price, Some(121));
    assert_eq!((stats.open_24h, stats.change_24h), (Some(120), Some(1)));
    assert_eq!((stats.high_24h, stats.low_24h), (Some(121), Some(120)));
    assert_eq!((stats.volume_lots_24h, stats.trades_24h), (65, 3));
    assert_eq!(stats.volume_quote_24h, 4_800 + 2_420 + 605);

    // Candles over the last hour: every lot is in one, the last close is 1.21.
    let now = Timestamp::now().as_micros();
    let candles: Vec<Candle> = env
        .dex(BOB, "get_candles", CandlesRequest {
            market: None,
            interval: CandleInterval::M1,
            from: now - 3_600_000_000,
            to: now + 60_000_000,
        })
        .await;
    assert_eq!(candles.iter().map(|c| c.volume_lots).sum::<u64>(), 65);
    assert_eq!(candles.last().map(|c| c.close), Some(121));
    assert!(candles.iter().all(|c| c.low <= c.open.min(c.close) && c.high >= c.open.max(c.close)));

    // A range too wide for one request is refused.
    let too_wide: ConductorApiResult<Vec<Candle>> = env
        .dex_fallible(BOB, "get_candles", CandlesRequest {
            market: None,
            interval: CandleInterval::M1,
            from: 0,
            to: now,
        })
        .await;
    assert!(too_wide.is_err());
    env.assert_supply().await;
}

/// The next `OrderUpdated` on this stream, skipping other signals.
async fn next_order_update(signals: &mut Signals) -> dex_api::DexSignal {
    loop {
        let s = next_dex_signal(signals).await;
        if matches!(s, dex_api::DexSignal::OrderUpdated { .. }) {
            return s;
        }
    }
}

/// The maker's UI hears the order's new status after each of its runs: a
/// partial fill, then a cancel. (The poll derives the same, so a lost signal
/// only delays the notification.)
#[tokio::test(flavor = "multi_thread")]
async fn order_updates_follow_each_run() {
    use dex_api::DexSignal;
    let env = TestEnv::new(2).await;
    let mut alice_signals = env.signals(ALICE);
    let _: ActionHash = env.call(ALICE, "mint", Amounts::new(4_000, 0)).await;
    let _: ActionHash = env.call(BOB, "mint", Amounts::new(0, 1_200)).await;
    let order: ActionHash = env.dex(ALICE, "place_order", place(sell(40, 120))).await;
    env.sync().await;
    let _: TakeResult = env
        .dex(BOB, "take", TakeRequest { market: None, take: Side::Buy, lots: 10, limit_price: None })
        .await;
    env.sync().await;

    let fill: Vec<RunReport> = env.dex(ALICE, "run_my_orders", ()).await;
    assert_eq!(
        next_order_update(&mut alice_signals).await,
        DexSignal::OrderUpdated { escrow: order.clone(), run: fill[0].run.clone(), status: OrderStatus::Partial, filled_lots: 10, lots: 40 }
    );
    let cancel: Vec<RunReport> = env.dex(ALICE, "cancel_order", order.clone()).await;
    assert_eq!(
        next_order_update(&mut alice_signals).await,
        DexSignal::OrderUpdated { escrow: order.clone(), run: cancel[0].run.clone(), status: OrderStatus::Cancelled, filled_lots: 10, lots: 40 }
    );
    env.assert_supply().await;
}
