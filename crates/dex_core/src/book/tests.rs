use super::*;

const ALICE: &str = "alice";
const BOB: &str = "bob";
const CAROL: &str = "carol";
const DAVE: &str = "dave";
const NOW: i64 = 1_000_000;
const LATER: i64 = 2_000_000;

fn order(id: u32, maker: &'static str, side: Side, price: u64, lots: u64, at: i64) -> OrderView<u32, &'static str> {
    OrderView {
        id,
        maker,
        side,
        price_per_lot: price,
        remaining_lots: lots,
        opened_at: at,
        expires_at: LATER,
        closed: false,
    }
}

fn ask(id: u32, maker: &'static str, price: u64, lots: u64, at: i64) -> OrderView<u32, &'static str> {
    order(id, maker, Side::Sell, price, lots, at)
}

fn bid(id: u32, maker: &'static str, price: u64, lots: u64, at: i64) -> OrderView<u32, &'static str> {
    order(id, maker, Side::Buy, price, lots, at)
}

fn level(price: u64, lots: u64, orders: usize) -> PriceLevel {
    PriceLevel {
        price_per_lot: price,
        lots,
        orders,
    }
}

fn planned(plan: &TakePlan<u32>) -> Vec<(u32, u64)> {
    plan.fills.iter().map(|f| (f.order, f.lots)).collect()
}

#[test]
fn orders_at_one_price_merge_into_a_level() {
    // §10: 40 + 35 + 25 at 1.20 shows as one level of 100.
    let orders = [
        ask(1, ALICE, 120, 40, NOW - 30),
        ask(2, BOB, 120, 35, NOW - 20),
        ask(3, CAROL, 120, 25, NOW - 10),
    ];
    let book = aggregate(&orders, NOW);
    assert_eq!(book.asks, vec![level(120, 100, 3)]);
    assert!(book.bids.is_empty());
    assert_eq!(book.spread, None, "no spread with one side empty");
}

#[test]
fn asks_ascend_bids_descend_and_spread_is_best_ask_minus_best_bid() {
    let orders = [
        ask(1, ALICE, 130, 10, NOW),
        ask(2, ALICE, 121, 10, NOW),
        bid(3, BOB, 110, 10, NOW),
        bid(4, BOB, 118, 10, NOW),
        bid(5, CAROL, 110, 5, NOW),
    ];
    let book = aggregate(&orders, NOW);
    assert_eq!(book.asks, vec![level(121, 10, 1), level(130, 10, 1)]);
    assert_eq!(book.bids, vec![level(118, 10, 1), level(110, 15, 2)]);
    assert_eq!(book.spread, Some(3));
}

#[test]
fn a_crossed_book_has_a_negative_spread() {
    let orders = [ask(1, ALICE, 115, 10, NOW), bid(2, BOB, 120, 10, NOW)];
    assert_eq!(aggregate(&orders, NOW).spread, Some(-5));
}

#[test]
fn expired_empty_and_closed_orders_are_not_live() {
    let mut expired = ask(1, ALICE, 120, 10, NOW - 10);
    expired.expires_at = NOW; // expiry is inclusive, as in `is_expired_at`
    let empty = ask(2, BOB, 120, 0, NOW - 10);
    let mut closed = ask(3, CAROL, 120, 10, NOW - 10);
    closed.closed = true;
    let open = ask(4, DAVE, 120, 7, NOW - 10);
    let orders = [expired, empty, closed, open];

    assert_eq!(live(&orders, NOW).map(|o| o.id).collect::<Vec<_>>(), vec![4]);
    assert_eq!(aggregate(&orders, NOW).asks, vec![level(120, 7, 1)]);
    let plan = plan_take(&orders, &MarketDef::default_pair(), &BOB, Side::Buy, 50, None, NOW).unwrap();
    assert_eq!(planned(&plan), vec![(4, 7)]);
}

#[test]
fn orders_at_a_level_come_in_time_priority_with_id_tiebreak() {
    let orders = [
        ask(9, CAROL, 120, 25, NOW - 10),
        ask(5, BOB, 120, 35, NOW - 20),
        ask(3, DAVE, 120, 5, NOW - 20),
        ask(1, ALICE, 125, 40, NOW - 30),
    ];
    let at_level: Vec<u32> = orders_at_level(&orders, Side::Sell, 120, NOW)
        .iter()
        .map(|o| o.id)
        .collect();
    assert_eq!(at_level, vec![3, 5, 9]);
    assert!(orders_at_level(&orders, Side::Buy, 120, NOW).is_empty());
}

#[test]
fn take_walks_one_level_in_time_priority() {
    // §13: a 60 A buy consumes Alice's 40 first, then 20 of Bob's 35.
    let orders = [
        ask(2, BOB, 120, 35, NOW - 20),
        ask(1, ALICE, 120, 40, NOW - 30),
        ask(3, CAROL, 120, 25, NOW - 10),
    ];
    let plan = plan_take(&orders, &MarketDef::default_pair(), &DAVE, Side::Buy, 60, None, NOW).unwrap();
    assert_eq!(planned(&plan), vec![(1, 40), (2, 20)]);
    assert_eq!(plan.fills[0].cost, Amounts::new(0, 4_800), "40 × 1.20 B");
    assert_eq!(plan.fills[1].cost, Amounts::new(0, 2_400), "20 × 1.20 B");
    assert_eq!(plan.total_cost, Amounts::new(0, 7_200));
    assert_eq!(plan.total_receives, Amounts::new(6_000, 0));
    assert_eq!((plan.filled, plan.shortfall), (60, 0));
}

#[test]
fn take_walks_best_price_first_across_levels() {
    let orders = [
        ask(1, ALICE, 125, 10, NOW - 30),
        ask(2, BOB, 120, 10, NOW - 10),
        ask(3, CAROL, 130, 10, NOW - 40),
    ];
    let plan = plan_take(&orders, &MarketDef::default_pair(), &DAVE, Side::Buy, 25, None, NOW).unwrap();
    assert_eq!(planned(&plan), vec![(2, 10), (1, 10), (3, 5)]);
    assert_eq!(plan.total_cost, Amounts::new(0, 1_200 + 1_250 + 650));
}

#[test]
fn take_respects_the_limit_price_and_reports_the_shortfall() {
    let orders = [
        ask(1, ALICE, 120, 10, NOW),
        ask(2, BOB, 121, 10, NOW),
        ask(3, CAROL, 122, 10, NOW),
    ];
    let plan = plan_take(&orders, &MarketDef::default_pair(), &DAVE, Side::Buy, 25, Some(121), NOW).unwrap();
    assert_eq!(planned(&plan), vec![(1, 10), (2, 10)]);
    assert_eq!((plan.filled, plan.shortfall), (20, 5));

    let nothing = plan_take(&orders, &MarketDef::default_pair(), &DAVE, Side::Buy, 5, Some(119), NOW).unwrap();
    assert!(nothing.fills.is_empty());
    assert_eq!((nothing.filled, nothing.shortfall), (0, 5));
    assert_eq!(nothing.total_cost, Amounts::ZERO);
}

#[test]
fn a_book_too_thin_for_the_request_reports_the_shortfall() {
    let orders = [ask(1, ALICE, 120, 30, NOW)];
    let plan = plan_take(&orders, &MarketDef::default_pair(), &BOB, Side::Buy, 100, None, NOW).unwrap();
    assert_eq!(planned(&plan), vec![(1, 30)]);
    assert_eq!((plan.filled, plan.shortfall), (30, 70));
}

#[test]
fn taker_sell_consumes_bids_highest_first_and_pays_in_a() {
    let orders = [
        bid(1, ALICE, 110, 10, NOW - 30),
        bid(2, BOB, 118, 10, NOW - 10),
        bid(3, CAROL, 115, 10, NOW - 20),
        ask(4, CAROL, 100, 50, NOW), // asks are ignored by a sell
    ];
    let plan = plan_take(&orders, &MarketDef::default_pair(), &DAVE, Side::Sell, 25, Some(112), NOW).unwrap();
    assert_eq!(planned(&plan), vec![(2, 10), (3, 10)]);
    assert_eq!((plan.filled, plan.shortfall), (20, 5));
    // The taker parks A (one lot = 1.00 A) and receives B at each bid's price.
    assert_eq!(plan.fills[0].cost, Amounts::new(1_000, 0));
    assert_eq!(plan.fills[0].receives, Amounts::new(0, 1_180));
    assert_eq!(plan.total_cost, Amounts::new(2_000, 0));
    assert_eq!(plan.total_receives, Amounts::new(0, 1_180 + 1_150));
}

#[test]
fn taker_skips_their_own_orders() {
    let orders = [ask(1, BOB, 119, 10, NOW - 30), ask(2, ALICE, 120, 10, NOW - 10)];
    let plan = plan_take(&orders, &MarketDef::default_pair(), &BOB, Side::Buy, 15, None, NOW).unwrap();
    assert_eq!(planned(&plan), vec![(2, 10)]);
    assert_eq!(plan.shortfall, 5);
    // The book itself still shows them.
    assert_eq!(aggregate(&orders, NOW).asks.len(), 2);
}

#[test]
fn plan_costs_match_what_a_settlement_run_charges() {
    // Parking a planned fill's cost must fill exactly the planned lots.
    let orders = [ask(1, ALICE, 120, 100, NOW - 30)];
    let plan = plan_take(&orders, &MarketDef::default_pair(), &BOB, Side::Buy, 40, None, NOW).unwrap();
    let terms = OrderTerms {
        side: Side::Sell,
        price_per_lot: 120,
        lots: 100,
        expires_at: LATER,
    };
    let out = crate::execute_run(&crate::RunInput {
        terms,
        market: MarketDef::default_pair(),
        maker: ALICE,
        prev_locked: terms.initial_lock(&MarketDef::default_pair()).unwrap(),
        parks: vec![crate::ParkInput {
            id: 7u32,
            taker: BOB,
            amounts: plan.fills[0].cost.clone(),
            requested_lots: plan.fills[0].lots,
            parked_at: NOW,
        }],
        now: NOW,
        mode: crate::RunMode::Fill,
    })
    .unwrap();
    assert_eq!(out.filled_lots, 40);
    let bob_gets = out.allocations.iter().find(|a| a.receiver == BOB).unwrap().amounts.clone();
    assert_eq!(bob_gets, plan.fills[0].receives, "no refund: the cost was exact");
}

/// xorshift64: deterministic pseudo-randomness without a dependency.
struct Rng(u64);

impl Rng {
    fn next(&mut self, bound: u64) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0 % bound
    }
}

#[test]
fn randomised_plans_never_exceed_the_request_or_any_level() {
    let makers = [ALICE, BOB, CAROL, DAVE];
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    for _ in 0..500 {
        let orders: Vec<_> = (0..rng.next(12) as u32)
            .map(|id| {
                let side = if rng.next(2) == 0 { Side::Sell } else { Side::Buy };
                let mut o = order(
                    id,
                    makers[rng.next(4) as usize],
                    side,
                    115 + rng.next(10),
                    rng.next(50),
                    NOW - rng.next(100) as i64,
                );
                o.closed = rng.next(8) == 0;
                o.expires_at = if rng.next(8) == 0 { NOW } else { LATER };
                o
            })
            .collect();
        let taker = makers[rng.next(4) as usize];
        let take = if rng.next(2) == 0 { Side::Buy } else { Side::Sell };
        let lots = rng.next(120);
        let limit = if rng.next(2) == 0 { None } else { Some(115 + rng.next(10)) };

        let plan = plan_take(&orders, &MarketDef::default_pair(), &taker, take, lots, limit, NOW).unwrap();
        assert_eq!(plan.filled + plan.shortfall, lots);
        assert_eq!(plan.fills.iter().map(|f| f.lots).sum::<u64>(), plan.filled);

        let book = aggregate(&orders, NOW);
        let levels = match take {
            Side::Buy => &book.asks,
            Side::Sell => &book.bids,
        };
        for fill in &plan.fills {
            assert!(fill.lots > 0);
            let source = orders.iter().find(|o| o.id == fill.order).unwrap();
            assert!(source.is_live(NOW) && source.maker != taker);
            assert!(fill.lots <= source.remaining_lots);
            if let Some(limit) = limit {
                match take {
                    Side::Buy => assert!(fill.price_per_lot <= limit),
                    Side::Sell => assert!(fill.price_per_lot >= limit),
                }
            }
            let at_price: u64 = plan
                .fills
                .iter()
                .filter(|f| f.price_per_lot == fill.price_per_lot)
                .map(|f| f.lots)
                .sum();
            let level = levels.iter().find(|l| l.price_per_lot == fill.price_per_lot).unwrap();
            assert!(at_price <= level.lots);
        }
    }
}

#[test]
fn order_status_follows_fills_release_and_expiry() {
    use OrderStatus::*;
    let status = |filled, released_at, now| order_status(100, filled, LATER, released_at, now);
    assert_eq!(status(0, None, NOW), Open);
    assert_eq!(status(40, None, NOW), Partial);
    assert_eq!(status(100, None, NOW), Filled);
    assert_eq!(status(100, Some(NOW), NOW), Filled, "a release after a full fill returns nothing");
    assert_eq!(status(0, Some(NOW), NOW), Cancelled);
    assert_eq!(status(40, Some(NOW), LATER + 5), Cancelled, "cancelled before expiry stays cancelled");
    assert_eq!(status(40, None, LATER), Expired, "expired, awaiting release");
    assert_eq!(status(0, Some(LATER), LATER + 5), Expired, "released at or after expiry");
}
