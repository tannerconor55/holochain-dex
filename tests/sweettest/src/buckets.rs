//! Day-bucketed listings, the order lifetime, listing deletes and the trade
//! index, against real validation (design `docs/design/read-performance.md`).

use super::*;
use dex_api::{RawTradeIndex, Trade};
use dex_core::buckets::{utc_day, TradeTag, DAY_US};

const SEVEN_DAYS_US: i64 = 7 * DAY_US;

fn raw_listing(escrow: &ActionHash, tag: Vec<u8>, day: Option<i64>) -> RawListing {
    RawListing { escrow: escrow.clone(), tag, anchor_market: None, anchor_day: day }
}

/// A listing must hang off its escrow's creation day and live at most seven
/// days; the right day, within the lifetime, passes.
#[tokio::test(flavor = "multi_thread")]
async fn listings_must_use_the_creation_day_and_the_lifetime() {
    let env = TestEnv::new(2).await;
    let _: ActionHash = env.call(ALICE, "mint", Amounts::new(20_000, 0)).await;

    // Escrows opened through the ledger directly (it knows no lifetime).
    let too_long = OrderTerms { expires_at: Timestamp::now().as_micros() + SEVEN_DAYS_US + 60_000_000, ..sell(10, 120) };
    let long_escrow: ActionHash = env.call(ALICE, "open_escrow", open(too_long)).await;
    let ok_escrow: ActionHash = env.call(ALICE, "open_escrow", open(sell(10, 121))).await;
    let ok_state: EscrowState = env.call(ALICE, "get_escrow_state", ok_escrow.clone()).await;
    let day = utc_day(ok_state.opened_at.as_micros());

    let long_tag = dex_core::listing::encode_tag(&demo_market(), &too_long);
    let refused: ConductorApiResult<ActionHash> =
        env.dex_fallible(ALICE, "list_escrow_raw", raw_listing(&long_escrow, long_tag, None)).await;
    assert!(format!("{refused:?}").contains("at most 604800 s"), "over the lifetime: {refused:?}");

    let tag = dex_core::listing::encode_tag(&demo_market(), &ok_state.terms);
    for wrong in [day - 1, day + 1] {
        let refused: ConductorApiResult<ActionHash> =
            env.dex_fallible(ALICE, "list_escrow_raw", raw_listing(&ok_escrow, tag.clone(), Some(wrong))).await;
        assert!(format!("{refused:?}").contains("creation day"), "day {wrong}: {refused:?}");
    }
    let _: ActionHash = env.dex(ALICE, "list_escrow_raw", raw_listing(&ok_escrow, tag, Some(day))).await;
    env.sync().await;
    assert_eq!(env.book(BOB).await.asks, vec![level(121, 10, 1)]);
    env.assert_supply().await;
}

/// Only the maker may delete a listing; trade index links are permanent.
#[tokio::test(flavor = "multi_thread")]
async fn only_the_maker_unlists_and_the_trade_index_is_permanent() {
    let env = TestEnv::new(2).await;
    let _: ActionHash = env.call(ALICE, "mint", Amounts::new(4_000, 0)).await;
    let order: ActionHash = env.dex(ALICE, "place_order", place(sell(40, 120))).await;
    env.sync().await;
    let links: Vec<ActionHash> = env.dex(BOB, "get_listing_links", order.clone()).await;
    assert_eq!(links.len(), 1);

    let stranger: ConductorApiResult<ActionHash> = env.dex_fallible(BOB, "unlist_raw", links[0].clone()).await;
    assert!(format!("{stranger:?}").contains("only the maker who listed"), "{stranger:?}");
    let _: ActionHash = env.dex(ALICE, "unlist_raw", links[0].clone()).await;
    env.sync().await;
    assert!(env.book(BOB).await.asks.is_empty(), "an unlisted order leaves the book");
    env.assert_supply().await;
}

/// The trade index tag must be exactly the run's trade, by the run's maker,
/// under the run's day; anything else is refused.
#[tokio::test(flavor = "multi_thread")]
async fn trade_index_links_must_match_their_run() {
    let env = TestEnv::new(2).await;
    let _: ActionHash = env.call(ALICE, "mint", Amounts::new(4_000, 0)).await;
    let _: ActionHash = env.call(BOB, "mint", Amounts::new(0, 4_800)).await;
    let order: ActionHash = env.dex(ALICE, "place_order", place(sell(40, 120))).await;
    env.sync().await;
    let _: ActionHash = env.call(BOB, "park", park_request(&order, 1_200, 10)).await;
    env.sync().await;
    let fill: Option<RunReport> = env.call(ALICE, "run_escrow", RunEscrowInput { escrow: order.clone(), mode: RunMode::Fill }).await;
    let run = fill.expect("a run").run;
    env.sync().await;

    // The run's trade, as the ledger derives it.
    let trades: Vec<Trade> = env.call(ALICE, "get_escrow_trades", order.clone()).await;
    let t = trades.into_iter().next().expect("one trade");
    let good = TradeTag {
        price_per_lot: 120,
        lots: 10,
        maker_side: Side::Sell,
        run_ts: t.timestamp,
        escrow: order.get_raw_39().to_vec(),
    };
    let day = utc_day(t.timestamp);
    let index = |tag: &TradeTag, day: i64| RawTradeIndex { run: run.clone(), market: demo_market(), day, tag: tag.encode() };

    let forged = [
        ("lots", TradeTag { lots: 11, ..good.clone() }, day),
        ("price", TradeTag { price_per_lot: 119, ..good.clone() }, day),
        ("side", TradeTag { maker_side: Side::Buy, ..good.clone() }, day),
        ("time", TradeTag { run_ts: t.timestamp - 1, ..good.clone() }, day),
        ("escrow", TradeTag { escrow: run.get_raw_39().to_vec(), ..good.clone() }, day),
        ("day", good.clone(), day + 1),
    ];
    for (what, tag, day) in forged {
        let r: ConductorApiResult<ActionHash> = env.dex_fallible(ALICE, "index_trade_raw", index(&tag, day)).await;
        assert!(r.is_err(), "a forged {what} must be refused");
    }
    let by_taker: ConductorApiResult<ActionHash> = env.dex_fallible(BOB, "index_trade_raw", index(&good, day)).await;
    assert!(format!("{by_taker:?}").contains("only the run's maker"), "{by_taker:?}");

    let link: ActionHash = env.dex(ALICE, "index_trade_raw", index(&good, day)).await;
    let delete: ConductorApiResult<ActionHash> = env.dex_fallible(ALICE, "unlist_raw", link).await;
    assert!(format!("{delete:?}").contains("permanent"), "{delete:?}");

    // A release sells nothing: no trade to index.
    let release: Option<RunReport> = env.call(ALICE, "run_escrow", RunEscrowInput { escrow: order.clone(), mode: RunMode::Release }).await;
    let release = release.expect("a release").run;
    let zero = RawTradeIndex { run: release, market: demo_market(), day: utc_day(Timestamp::now().as_micros()), tag: good.encode() };
    let r: ConductorApiResult<ActionHash> = env.dex_fallible(ALICE, "index_trade_raw", zero).await;
    assert!(r.is_err(), "a release is not a trade");
    env.assert_supply().await;
}
