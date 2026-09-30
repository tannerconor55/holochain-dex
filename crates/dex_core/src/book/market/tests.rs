use super::*;
use crate::book::OrderView;

const ALICE: &str = "alice";
const BOB: &str = "bob";
const CAROL: &str = "carol";
const DAVE: &str = "dave";
const NOW: i64 = 1_000_000;
const LATER: i64 = 2_000_000;

type View = OrderView<u32, &'static str>;

fn order(id: u32, maker: &'static str, side: Side, price: u64, lots: u64, at: i64) -> View {
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

fn ask(id: u32, maker: &'static str, price: u64, lots: u64) -> View {
    order(id, maker, Side::Sell, price, lots, NOW - 100 + i64::from(id))
}

fn bid(id: u32, maker: &'static str, price: u64, lots: u64) -> View {
    order(id, maker, Side::Buy, price, lots, NOW - 100 + i64::from(id))
}

fn fills(m: &MarketPlan<u32>) -> Vec<(u32, u64)> {
    m.plan.fills.iter().map(|f| (f.order, f.lots)).collect()
}

#[test]
fn an_empty_book_is_an_error_not_a_plan() {
    let none: Vec<View> = vec![];
    assert_eq!(plan_market(&none, &MarketDef::default_pair(), Side::Buy, 10, 200, &DAVE, NOW), Err(MarketError::EmptyBook));
    // Only bids: nothing for a market buy to take.
    let bids = [bid(1, ALICE, 110, 10)];
    assert_eq!(plan_market(&bids, &MarketDef::default_pair(), Side::Buy, 10, 200, &DAVE, NOW), Err(MarketError::EmptyBook));
    assert_eq!(
        plan_market_by_budget(&bids, &MarketDef::default_pair(), Side::Buy, 10_000, 200, &DAVE, NOW),
        Err(MarketError::EmptyBook)
    );
    assert_eq!(
        plan_market(&[ask(1, ALICE, 120, 10)], &MarketDef::default_pair(), Side::Buy, 0, 200, &DAVE, NOW),
        Err(MarketError::NothingToTake)
    );
}

#[test]
fn one_level_fills_at_that_price() {
    let orders = [ask(1, ALICE, 120, 100)];
    let m = plan_market(&orders, &MarketDef::default_pair(), Side::Buy, 40, 200, &BOB, NOW).unwrap();
    assert_eq!(fills(&m), vec![(1, 40)]);
    assert_eq!(m.plan.total_cost, Amounts::new(0, 4_800));
    assert_eq!((m.total_quote_minor, m.total_lots), (4_800, 40), "average 1.20");
    assert_eq!(m.worst_price, Some(120));
    assert_eq!(m.reference_price, Some(120));
    assert_eq!(m.limit_price, 123, "ceil(120 × 1.02) = ceil(122.4)");
    assert_eq!((m.plan.filled, m.plan.shortfall), (40, 0));
}

#[test]
fn a_sweep_crosses_levels_best_first() {
    let orders = [ask(1, ALICE, 121, 10), ask(2, BOB, 120, 10), ask(3, CAROL, 122, 10)];
    let m = plan_market(&orders, &MarketDef::default_pair(), Side::Buy, 25, 200, &DAVE, NOW).unwrap();
    assert_eq!(fills(&m), vec![(2, 10), (1, 10), (3, 5)]);
    assert_eq!(m.total_quote_minor, 1_200 + 1_210 + 610);
    assert_eq!(m.total_lots, 25);
    assert_eq!(m.plan.total_cost, Amounts::new(0, m.total_quote_minor));
    assert_eq!(m.worst_price, Some(122));
}

#[test]
fn the_slippage_limit_stops_the_sweep() {
    // Best 1.00, 2% → limit 1.02: the 1.03 level is out of reach.
    let orders = [ask(1, ALICE, 100, 10), ask(2, BOB, 102, 10), ask(3, CAROL, 103, 50)];
    let m = plan_market(&orders, &MarketDef::default_pair(), Side::Buy, 40, 200, &DAVE, NOW).unwrap();
    assert_eq!(m.limit_price, 102);
    assert_eq!(fills(&m), vec![(1, 10), (2, 10)]);
    assert_eq!((m.plan.filled, m.plan.shortfall), (20, 20));
    // A wider allowance reaches it.
    let wide = plan_market(&orders, &MarketDef::default_pair(), Side::Buy, 40, 300, &DAVE, NOW).unwrap();
    assert_eq!(fills(&wide), vec![(1, 10), (2, 10), (3, 20)]);
}

#[test]
fn limit_rounding_at_the_boundary_for_both_sides() {
    // Exact products need no rounding.
    assert_eq!(market_limit(Side::Buy, 100, 200), Ok(102));
    assert_eq!(market_limit(Side::Sell, 100, 200), Ok(98));
    // Fractional products: buy rounds up, sell rounds down.
    assert_eq!(market_limit(Side::Buy, 120, 200), Ok(123), "122.4 → 123");
    assert_eq!(market_limit(Side::Sell, 120, 200), Ok(117), "117.6 → 117");
    assert_eq!(market_limit(Side::Buy, 1, 1), Ok(2), "1.0001 → 2");
    assert_eq!(market_limit(Side::Sell, 1, 1), Ok(0), "0.9999 → 0");
    // Zero allowance: exactly the best price.
    assert_eq!(market_limit(Side::Buy, 120, 0), Ok(120));
    assert_eq!(market_limit(Side::Sell, 120, 0), Ok(120));
    // Sell allowance above 100% would be negative.
    assert_eq!(market_limit(Side::Sell, 120, 10_000), Ok(0));
    assert_eq!(market_limit(Side::Sell, 120, 10_001), Err(MarketError::SlippageTooLarge));
    // No overflow for huge prices: the product is computed in u128.
    assert_eq!(market_limit(Side::Sell, u64::MAX, 0), Ok(u64::MAX));
    assert_eq!(market_limit(Side::Buy, u64::MAX, 1), Err(MarketError::Overflow));

    // A level exactly on the limit is taken; one minor unit past it is not.
    let buy_book = [ask(1, ALICE, 120, 1), ask(2, BOB, 123, 1), ask(3, CAROL, 124, 1)];
    let m = plan_market(&buy_book, &MarketDef::default_pair(), Side::Buy, 3, 200, &DAVE, NOW).unwrap();
    assert_eq!(fills(&m), vec![(1, 1), (2, 1)]);
    let sell_book = [bid(1, ALICE, 120, 1), bid(2, BOB, 117, 1), bid(3, CAROL, 116, 1)];
    let m = plan_market(&sell_book, &MarketDef::default_pair(), Side::Sell, 3, 200, &DAVE, NOW).unwrap();
    assert_eq!(fills(&m), vec![(1, 1), (2, 1)]);
}

#[test]
fn a_market_sell_sweeps_bids_highest_first() {
    let orders = [bid(1, ALICE, 118, 10), bid(2, BOB, 120, 10), bid(3, CAROL, 110, 10)];
    let m = plan_market(&orders, &MarketDef::default_pair(), Side::Sell, 15, 200, &DAVE, NOW).unwrap();
    assert_eq!(m.limit_price, 117);
    assert_eq!(fills(&m), vec![(2, 10), (1, 5)]);
    assert_eq!(m.plan.total_cost, Amounts::new(1_500, 0), "the taker pays in A");
    assert_eq!(m.plan.total_receives, Amounts::new(0, 1_200 + 590));
    assert_eq!((m.total_quote_minor, m.total_lots), (1_790, 15));
    assert_eq!(m.worst_price, Some(118));
}

#[test]
fn the_taker_skips_their_own_orders_including_for_the_reference_price() {
    // Dave's own 1.00 ask must not set the reference price.
    let orders = [ask(1, DAVE, 100, 50), ask(2, ALICE, 120, 10), ask(3, BOB, 122, 10)];
    let m = plan_market(&orders, &MarketDef::default_pair(), Side::Buy, 15, 200, &DAVE, NOW).unwrap();
    assert_eq!(m.reference_price, Some(120));
    assert_eq!(m.limit_price, 123);
    assert_eq!(fills(&m), vec![(2, 10), (3, 5)]);
    // A book of only the taker's own orders is empty to them.
    assert_eq!(
        plan_market(&[ask(1, DAVE, 100, 5)], &MarketDef::default_pair(), Side::Buy, 1, 200, &DAVE, NOW),
        Err(MarketError::EmptyBook)
    );
}

#[test]
fn expired_and_closed_orders_are_skipped() {
    let mut expired = ask(1, ALICE, 100, 10);
    expired.expires_at = NOW;
    let mut closed = ask(2, BOB, 100, 10);
    closed.closed = true;
    let orders = [expired, closed, ask(3, CAROL, 120, 10)];
    let m = plan_market(&orders, &MarketDef::default_pair(), Side::Buy, 5, 200, &DAVE, NOW).unwrap();
    assert_eq!(m.reference_price, Some(120), "dead orders do not set the reference price");
    assert_eq!(fills(&m), vec![(3, 5)]);
}

#[test]
fn a_budget_buys_whole_lots_and_is_never_exceeded() {
    let orders = [ask(1, ALICE, 120, 10), ask(2, BOB, 121, 10)];
    // 20.00 B: 10 lots at 1.20 (12.00 B), then 6 at 1.21 (7.26 B) = 19.26 B.
    let m = plan_market_by_budget(&orders, &MarketDef::default_pair(), Side::Buy, 2_000, 200, &DAVE, NOW).unwrap();
    assert_eq!(fills(&m), vec![(1, 10), (2, 6)]);
    assert_eq!(m.plan.total_cost, Amounts::new(0, 1_926));
    assert_eq!(m.unspent_budget, Some(74));
    // Exactly enough for one lot.
    let one = plan_market_by_budget(&orders, &MarketDef::default_pair(), Side::Buy, 120, 200, &DAVE, NOW).unwrap();
    assert_eq!(fills(&one), vec![(1, 1)]);
    assert_eq!(one.unspent_budget, Some(0));
    // Less than one lot.
    assert_eq!(
        plan_market_by_budget(&orders, &MarketDef::default_pair(), Side::Buy, 119, 200, &DAVE, NOW),
        Err(MarketError::NothingToTake)
    );
    // Selling by budget spends A, 1.00 A per lot.
    let bids = [bid(1, ALICE, 120, 10)];
    let s = plan_market_by_budget(&bids, &MarketDef::default_pair(), Side::Sell, 350, 200, &DAVE, NOW).unwrap();
    assert_eq!(fills(&s), vec![(1, 3)]);
    assert_eq!(s.unspent_budget, Some(50));
}

#[test]
fn a_budget_stops_at_the_slippage_limit_too() {
    let orders = [ask(1, ALICE, 100, 5), ask(2, BOB, 110, 100)];
    let m = plan_market_by_budget(&orders, &MarketDef::default_pair(), Side::Buy, 100_000, 200, &DAVE, NOW).unwrap();
    assert_eq!(fills(&m), vec![(1, 5)]);
    assert_eq!(m.unspent_budget, Some(100_000 - 500));
}

#[test]
fn a_retry_keeps_the_given_limit_and_tolerates_an_empty_book() {
    let orders = [ask(1, ALICE, 125, 10), ask(2, BOB, 123, 10)];
    // The original limit was 1.23: the new 1.25 level stays out of reach even
    // though a fresh 2% from the new best price (1.23) would allow it.
    let m = plan_with_limit(&orders, &MarketDef::default_pair(), Side::Buy, 15, 123, &DAVE, NOW).unwrap();
    assert_eq!(fills(&m), vec![(2, 10)]);
    assert_eq!((m.plan.shortfall, m.max_slippage_bps), (5, None));
    let none: Vec<View> = vec![];
    let empty = plan_with_limit(&none, &MarketDef::default_pair(), Side::Buy, 5, 123, &DAVE, NOW).unwrap();
    assert_eq!((empty.plan.filled, empty.plan.shortfall, empty.reference_price), (0, 5, None));
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
fn randomised_costs_match_lots_times_price_and_respect_budget_and_limit() {
    let makers = [ALICE, BOB, CAROL, DAVE];
    let mut rng = Rng(0xD1B5_4A32_D192_ED03);
    let mut planned = 0;
    for _ in 0..2_000 {
        let orders: Vec<View> = (0..rng.next(12) as u32)
            .map(|id| {
                let side = if rng.next(2) == 0 { Side::Sell } else { Side::Buy };
                let mut o = order(id, makers[rng.next(4) as usize], side, 90 + rng.next(40), rng.next(30), NOW - rng.next(50) as i64);
                o.closed = rng.next(10) == 0;
                o.expires_at = if rng.next(10) == 0 { NOW } else { LATER };
                o
            })
            .collect();
        let taker = makers[rng.next(4) as usize];
        let take = if rng.next(2) == 0 { Side::Buy } else { Side::Sell };
        let bps = rng.next(600) as u32;
        let by_budget = rng.next(2) == 0;
        let budget = rng.next(20_000);
        let lots = 1 + rng.next(80);

        let result = if by_budget {
            plan_market_by_budget(&orders, &MarketDef::default_pair(), take, budget, bps, &taker, NOW)
        } else {
            plan_market(&orders, &MarketDef::default_pair(), take, lots, bps, &taker, NOW)
        };
        let Ok(m) = result else { continue };
        planned += 1;

        let per_lot_paid = |price: u64| match take {
            Side::Buy => price,
            Side::Sell => 100,
        };
        let mut quote = 0;
        for f in &m.plan.fills {
            let o = orders.iter().find(|o| o.id == f.order).unwrap();
            assert!(o.is_live(NOW) && o.maker != taker && f.lots > 0 && f.lots <= o.remaining_lots);
            assert_eq!(f.price_per_lot, o.price_per_lot, "each maker's own price");
            assert!(within(take, f.price_per_lot, m.limit_price), "never past the limit");
            assert_eq!(pay(take, &f.cost, &MarketDef::default_pair()), f.lots * per_lot_paid(f.price_per_lot));
            quote += f.lots * f.price_per_lot;
        }
        assert_eq!(m.total_quote_minor, quote);
        assert_eq!(m.total_lots, m.plan.filled);
        assert_eq!(pay(take, &m.plan.total_cost, &MarketDef::default_pair()), m.plan.fills.iter().map(|f| pay(take, &f.cost, &MarketDef::default_pair())).sum::<u64>());
        if by_budget {
            let spent = pay(take, &m.plan.total_cost, &MarketDef::default_pair());
            assert!(spent <= budget, "budget {budget} exceeded: {spent}");
            assert_eq!(m.unspent_budget, Some(budget - spent));
        } else {
            assert_eq!(m.plan.filled + m.plan.shortfall, lots);
        }
    }
    assert!(planned > 500, "the generator should mostly produce plannable books");
}
