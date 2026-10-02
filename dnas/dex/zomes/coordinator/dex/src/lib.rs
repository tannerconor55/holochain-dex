//! # dex — order book coordinator
//!
//! Lists orders, reads the book and plans takes. Every movement of money is a
//! `call` to the `ledger` zome: this zome never reads or writes ledger entries,
//! so replacing the mock ledger with Unyt touches only the ledger's crates.
//!
//! Calls to `ledger` run in this call's workspace, so each extern here commits
//! atomically: `place_order`'s escrow and listing land together or not at all.
//!
//! The book is a view (§10): nothing computed here is stored. Book maths lives
//! in `dex_core::book`.

use dex_api::{
    BookView, Candle, CandlesRequest, DexConfig, DexSignal, MarketStats, RecentTradesRequest, Trade,
    MAX_RECENT_TRADES, FillChange, LevelQuery, MakerPresence, MarketAmount, MarketBudgetRequest,
    MarketInfo, MarketOrderRequest, MarketPlan, MarketPreviewRequest, MarketResult, MarketRetry, MyOrder,
    Order, PlaceOrderRequest, PlacedPark, SignalFill, RawListing, RawTradeIndex, RetryMarketRequest, TakePlan, TakeRequest,
    TakeResult, DEFAULT_MAX_SLIPPAGE_BPS,
};
use dex_core::book;
use dex_core::listing::{decode_tag, encode_tag, ListingTag};
use dex_core::properties::DexProperties;
use dex_core::{MarketDef, MarketId};
use dex_core::buckets::{live_listing_days, utc_day, TRADE_LOOKBACK_DAYS};
use dex_integrity::{listing_anchor, load_properties, LinkTypes};
use hdk::prelude::*;
use ledger_api::{
    EscrowState, OpenEscrowRequest, ParkRequest, ParkStatus, PendingPark, RunEscrowInput, RunMode, Side,
    RunReport,
};
use std::collections::{BTreeMap, BTreeSet};

const LEDGER: &str = "ledger";

/// Most settlement runs one call makes on one escrow. Each run consumes up to
/// `dex_core::MAX_PARKS_PER_RUN` parks; anything beyond waits for the next call.
const MAX_RUNS_PER_CALL: usize = 10;

// ---------------------------------------------------------------------------
// Signals
// ---------------------------------------------------------------------------

/// Let other agents send this cell remote signals and ping it, and nothing
/// else. One grant per function, so each can be revoked alone.
#[hdk_extern]
pub fn init() -> ExternResult<InitCallbackResult> {
    for (tag, function) in [("remote_signals", "recv_remote_signal"), ("presence", "ping")] {
        let mut functions = HashSet::new();
        functions.insert((zome_info()?.name, FunctionName::from(function)));
        create_cap_grant(CapGrantEntry {
            tag: tag.into(),
            access: CapAccess::Unrestricted,
            functions: GrantedFunctions::Listed(functions),
        })?;
    }
    Ok(InitCallbackResult::Pass)
}

/// Pass a signal from another agent to the local UI. Anyone may call this,
/// so it only re-emits: the UI treats every signal as a hint to re-read.
#[hdk_extern]
pub fn recv_remote_signal(signal: DexSignal) -> ExternResult<()> {
    emit_signal(signal)
}

/// Fire and forget: an offline recipient finds out on their next poll.
fn notify(signal: DexSignal, agents: Vec<AgentPubKey>) -> ExternResult<()> {
    if agents.is_empty() {
        return Ok(());
    }
    send_remote_signal(signal, agents)
}

// ---------------------------------------------------------------------------
// Maker presence (advisory)
// ---------------------------------------------------------------------------

/// Answers any agent: proof this cell is online right now. Reads nothing.
#[hdk_extern]
pub fn ping() -> ExternResult<()> {
    Ok(())
}

/// Ping the makers of `orders`, all at once, before a taker parks against
/// them. A park against an offline maker waits for them until its deadline;
/// this lets the UI warn first. Advisory: it proves nothing, and takes still
/// work without it. An unreachable maker costs up to the network request
/// timeout (60 s by default); pings run concurrently, so that is the most
/// this call waits.
#[hdk_extern]
pub fn check_makers(orders: Vec<ActionHash>) -> ExternResult<Vec<MakerPresence>> {
    let mut makers = BTreeMap::new();
    for order in &orders {
        let state: EscrowState = ledger("get_escrow_state", order.clone())?;
        makers.insert(order.clone(), state.maker);
    }
    let me = my_key()?;
    let distinct: Vec<AgentPubKey> = makers
        .values()
        .filter(|m| **m != me)
        .cloned()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let zome = zome_info()?.name;
    let calls = distinct
        .iter()
        .map(|agent| {
            Ok(Call::new(
                CallTarget::NetworkAgent(agent.clone()),
                zome.clone(),
                FunctionName::from("ping"),
                None,
                ExternIO::encode(()).map_err(|e| wasm_error!(e))?,
            ))
        })
        .collect::<ExternResult<Vec<_>>>()?;
    let responses = HDK.with(|h| h.borrow().call(calls))?;
    let mut seen = BTreeMap::new();
    for (agent, response) in distinct.into_iter().zip(responses) {
        seen.insert(agent, ping_outcome(response));
    }
    Ok(orders
        .into_iter()
        .filter_map(|order| {
            let maker = makers.get(&order)?.clone();
            // Your own order: you are online by definition.
            let detail = seen.get(&maker).cloned().unwrap_or(None);
            Some(MakerPresence { order, reachable: detail.is_none(), maker, detail })
        })
        .collect())
}

/// `None` if the ping answered, else why not.
fn ping_outcome(response: ZomeCallResponse) -> Option<String> {
    match response {
        ZomeCallResponse::Ok(_) => None,
        ZomeCallResponse::NetworkError(e) => Some(format!("not reachable: {e}")),
        ZomeCallResponse::Unauthorized(..) => Some("reachable but refused the ping (older app version?)".into()),
        ZomeCallResponse::AuthenticationFailed(..) => Some("authentication failed".into()),
        ZomeCallResponse::CountersigningSession(e) => Some(format!("busy in a countersigning session: {e}")),
    }
}

// ---------------------------------------------------------------------------
// Listing
// ---------------------------------------------------------------------------

/// The DNA's park timing and every market it declares.
#[hdk_extern]
pub fn get_config() -> ExternResult<DexConfig> {
    let props = props()?;
    let markets = props
        .markets
        .iter()
        .map(|def| {
            let decimals = |unit: &str| {
                props
                    .unit(unit)
                    .map(|u| u.decimals)
                    .ok_or_else(|| guest(format!("market unit {unit} is not declared")))
            };
            Ok(MarketInfo {
                id: def.id(),
                def: def.clone(),
                base_decimals: decimals(&def.base)?,
                quote_decimals: decimals(&def.quote)?,
            })
        })
        .collect::<ExternResult<Vec<_>>>()?;
    Ok(DexConfig {
        park_timeout_secs: props.park_timeout_secs,
        settle_grace_secs: props.settle_grace_secs,
        markets,
    })
}

/// Open an order's escrow and list it in the book. Returns the escrow hash,
/// which is the order's identity.
#[hdk_extern]
pub fn place_order(request: PlaceOrderRequest) -> ExternResult<ActionHash> {
    let (market, _) = market(request.market)?;
    let terms = request.terms;
    // Refuse before any funds are locked: the listing would be refused anyway
    // (and the escrow with it, in this one atomic call), but say why. The
    // escrow's timestamp will be at or after `now`, so this is the stricter test.
    let lifetime = max_lifetime_us()?;
    if terms.expires_at.saturating_sub(now()?) > lifetime {
        return Err(guest(format!(
            "an order may last at most {} days; choose an earlier expiry",
            lifetime / dex_core::buckets::DAY_US
        )));
    }
    let escrow: ActionHash = ledger("open_escrow", OpenEscrowRequest { market, terms })?;
    let day = utc_day(escrow_created_at(&escrow)?);
    list(escrow.clone(), &market, day, encode_tag(&market, &terms))?;
    Ok(escrow)
}

/// List an escrow the caller opened through the ledger directly. Validation
/// rejects it unless the caller is the escrow's maker.
#[hdk_extern]
pub fn republish_listing(escrow: ActionHash) -> ExternResult<ActionHash> {
    let state: EscrowState = ledger("get_escrow_state", escrow.clone())?;
    let day = utc_day(state.opened_at.as_micros());
    list(escrow, &state.market, day, encode_tag(&state.market, &state.terms))
}

/// List with a caller-chosen tag. Exists to test that validation rejects any
/// tag except the escrow's own; it cannot create a listing `place_order` could not.
#[hdk_extern]
pub fn list_escrow_raw(input: RawListing) -> ExternResult<ActionHash> {
    let state: EscrowState = ledger("get_escrow_state", input.escrow.clone())?;
    let market = input.anchor_market.unwrap_or(state.market);
    let day = input.anchor_day.unwrap_or_else(|| utc_day(state.opened_at.as_micros()));
    list(input.escrow, &market, day, input.tag)
}

/// Write a trade index link as given. Exists to test that validation rejects
/// every index link but a run's own trade, by its maker.
#[hdk_extern]
pub fn index_trade_raw(input: RawTradeIndex) -> ExternResult<ActionHash> {
    create_link(
        dex_integrity::trade_anchor(&input.market, input.day)?,
        input.run,
        LinkTypes::MarketTradesByDay,
        LinkTag::new(input.tag),
    )
}

/// Delete a dex link by its create action. Exists to test that only a
/// listing's maker may delete it and that index links cannot be deleted.
#[hdk_extern]
pub fn unlist_raw(create_link: ActionHash) -> ExternResult<ActionHash> {
    delete_link(create_link, GetOptions::network())
}

/// The listing links (create action hashes) of an escrow, from its creation
/// day's anchor. For tests and the maker's own unlisting.
#[hdk_extern]
pub fn get_listing_links(escrow: ActionHash) -> ExternResult<Vec<ActionHash>> {
    let state: EscrowState = ledger("get_escrow_state", escrow.clone())?;
    let anchor = listing_anchor(&state.market, utc_day(state.opened_at.as_micros()))?;
    Ok(get_links(LinkQuery::try_new(anchor, LinkTypes::MarketToOrders)?, GetStrategy::Network)?
        .into_iter()
        .filter(|l| l.target.clone().into_action_hash().as_ref() == Some(&escrow))
        .map(|l| l.create_link_hash)
        .collect())
}

/// List `escrow` under its market's anchor for UTC day `day`.
fn list(escrow: ActionHash, market: &MarketId, day: i64, tag: Vec<u8>) -> ExternResult<ActionHash> {
    create_link(listing_anchor(market, day)?, escrow, LinkTypes::MarketToOrders, LinkTag::new(tag))
}

/// When the caller's own escrow was written, read from their chain (this
/// call's writes included): its UTC day picks the listing anchor.
fn escrow_created_at(escrow: &ActionHash) -> ExternResult<i64> {
    own_action_timestamp(escrow)
}

/// Listing links in `market` created on `days`, deduplicated by escrow, with
/// their decoded tags. Readers still check every order's state.
fn listings(market: &MarketId, days: impl Iterator<Item = i64>) -> ExternResult<Vec<(ActionHash, ListingTag)>> {
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    for day in days {
        let links = get_links(
            LinkQuery::try_new(listing_anchor(market, day)?, LinkTypes::MarketToOrders)?,
            GetStrategy::Network,
        )?;
        for link in links {
            let Some(escrow) = link.target.into_action_hash() else { continue };
            let Some(tag) = decode_tag(&link.tag.0) else { continue };
            if &tag.market == market && seen.insert(escrow.clone()) {
                out.push((escrow, tag));
            }
        }
    }
    Ok(out)
}

fn max_lifetime_us() -> ExternResult<i64> {
    props()?.max_order_lifetime_us().map_err(|e| guest(e.to_string()))
}

/// The DNA properties; unusable properties are an error, never a default.
fn props() -> ExternResult<DexProperties> {
    load_properties()?.map_err(guest)
}

/// `id`'s market, or the first the DNA properties declare when `None`.
fn market(id: Option<MarketId>) -> ExternResult<(MarketId, MarketDef)> {
    let props = props()?;
    let def = match id {
        Some(id) => props.market(&id).cloned().map_err(|e| guest(e.to_string()))?,
        None => props
            .markets
            .first()
            .cloned()
            .ok_or_else(|| guest("the DNA properties declare no market"))?,
    };
    Ok((def.id(), def))
}

// ---------------------------------------------------------------------------
// Book reads
// ---------------------------------------------------------------------------

#[hdk_extern]
pub fn get_order_book(market_id: Option<MarketId>) -> ExternResult<BookView> {
    let now = now()?;
    let (id, _) = market(market_id)?;
    Ok(book::aggregate(&load_orders(&id, now)?, now))
}

/// The orders at one price level, in time priority.
#[hdk_extern]
pub fn get_level_orders(query: LevelQuery) -> ExternResult<Vec<Order>> {
    let now = now()?;
    let (id, _) = market(query.market)?;
    Ok(book::orders_at_level(&load_orders(&id, now)?, query.side, query.price_per_lot, now))
}

/// Preview a take. Read-only.
#[hdk_extern]
pub fn plan_take(request: TakeRequest) -> ExternResult<TakePlan<ActionHash>> {
    let now = now()?;
    let (id, def) = market(request.market)?;
    plan(&request, &def, &load_orders(&id, now)?, now)
}

fn plan(request: &TakeRequest, def: &MarketDef, orders: &[Order], now: i64) -> ExternResult<TakePlan<ActionHash>> {
    book::plan_take(orders, def, &my_key()?, request.take, request.lots, request.limit_price, now).map_err(core_err)
}

/// Every live listed order in `market`.
///
/// Cost: one `get_links` per day an order could still be live on (8 for a
/// 7-day lifetime), then one ledger state read per listing not already
/// expired by its tag. Orders whose state cannot be read yet (not gossiped)
/// are left out rather than failing the whole book.
fn load_orders(market: &MarketId, now: i64) -> ExternResult<Vec<Order>> {
    let mut orders = Vec::new();
    for (escrow, tag) in listings(market, live_listing_days(now, max_lifetime_us()?))? {
        if now >= tag.expires_at {
            continue;
        }
        let Ok(state) = ledger::<_, EscrowState>("get_escrow_state", escrow) else {
            continue;
        };
        orders.push(order_view(state));
    }
    Ok(orders)
}

fn order_view(state: EscrowState) -> Order {
    Order {
        id: state.escrow,
        maker: state.maker,
        side: state.terms.side,
        price_per_lot: state.terms.price_per_lot,
        remaining_lots: state.remaining_lots,
        opened_at: state.opened_at.as_micros(),
        expires_at: state.terms.expires_at,
        closed: state.closed,
    }
}

// ---------------------------------------------------------------------------
// Trade history and price data
// ---------------------------------------------------------------------------
//
// Read from the trade index: a link per sold run under a per-market, per-day
// anchor, whose tag validation has checked against the run. Cost: one
// `get_links` per day read, no per-trade or per-order reads.
// - recent trades: today, then earlier days until `limit` (at most
//   TRADE_LOOKBACK_DAYS);
// - stats: the two days the last 24 h touch; if they hold no trade, earlier
//   days for the last price, back TRADE_LOOKBACK_DAYS (beyond: unknown);
// - candles: the days the range covers (at most TRADE_LOOKBACK_DAYS + 1).
// A maker who skips the index link hides that trade from all three; it is
// informational and never affects settlement.

/// The `limit` most recent trades in a market, newest first.
#[hdk_extern]
pub fn get_recent_trades(request: RecentTradesRequest) -> ExternResult<Vec<Trade>> {
    let (id, _) = market(request.market)?;
    let limit = request.limit.min(MAX_RECENT_TRADES) as usize;
    let today = utc_day(now()?);
    let mut trades = Vec::new();
    for day in (today - TRADE_LOOKBACK_DAYS..=today).rev() {
        trades.extend(indexed_trades(&id, day)?);
        if trades.len() >= limit {
            break;
        }
    }
    dex_trades::sort_newest_first(&mut trades);
    trades.truncate(limit);
    Ok(trades)
}

/// Last price and 24-hour stats for a market.
#[hdk_extern]
pub fn get_market_stats(market_id: Option<MarketId>) -> ExternResult<MarketStats> {
    let (id, _) = market(market_id)?;
    let now = now()?;
    let since = now.saturating_sub(dex_trades::DAY_US);
    let mut trades = Vec::new();
    for day in dex_core::buckets::days_between(since, now + 1) {
        trades.extend(indexed_trades(&id, day)?);
    }
    // The last price can be older than the window: walk back for it.
    if !trades.iter().any(|t| t.timestamp > since && t.timestamp <= now) {
        for day in (utc_day(now) - TRADE_LOOKBACK_DAYS..utc_day(since)).rev() {
            let found = indexed_trades(&id, day)?;
            if !found.is_empty() {
                trades.extend(found);
                break;
            }
        }
    }
    dex_trades::market_stats(&trades, now).map_err(|e| guest(e.to_string()))
}

/// OHLC candles for a market, oldest first; intervals with no trade have no
/// candle. The range may span at most TRADE_LOOKBACK_DAYS + 1 days.
#[hdk_extern]
pub fn get_candles(request: CandlesRequest) -> ExternResult<Vec<Candle>> {
    let (id, _) = market(request.market)?;
    let days = dex_core::buckets::days_between(request.from, request.to);
    if days.clone().count() as i64 > TRADE_LOOKBACK_DAYS + 1 {
        return Err(guest(format!("a candle request may span at most {} days", TRADE_LOOKBACK_DAYS + 1)));
    }
    let mut trades = Vec::new();
    for day in days {
        trades.extend(indexed_trades(&id, day)?);
    }
    dex_trades::candles(&trades, request.interval, request.from, request.to).map_err(|e| guest(e.to_string()))
}

/// The trades indexed under `market` on UTC `day`, deduplicated by run.
fn indexed_trades(market: &MarketId, day: i64) -> ExternResult<Vec<Trade>> {
    let links = get_links(
        LinkQuery::try_new(dex_integrity::trade_anchor(market, day)?, LinkTypes::MarketTradesByDay)?,
        GetStrategy::Network,
    )?;
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    for link in links {
        let Some(run) = link.target.into_action_hash() else { continue };
        let Some(tag) = dex_core::buckets::TradeTag::decode(&link.tag.0) else { continue };
        let Ok(escrow) = ActionHash::try_from_raw_39(tag.escrow.clone()) else { continue };
        if !seen.insert(run.clone()) {
            continue;
        }
        out.push(Trade {
            market: *market,
            escrow,
            run,
            timestamp: tag.run_ts,
            price_per_lot: tag.price_per_lot,
            lots: tag.lots,
            maker_side: tag.maker_side,
            quote: tag.lots.saturating_mul(tag.price_per_lot),
        });
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Taking
// ---------------------------------------------------------------------------

/// Park funds against the best orders for `request`.
///
/// The plan is re-derived from fresh state, never taken from the caller, so a
/// stale UI cannot park against an order that has since filled. The maker's
/// next run settles each park; anything it cannot fill is refunded then.
#[hdk_extern]
pub fn take(request: TakeRequest) -> ExternResult<TakeResult> {
    let now = now()?;
    let (id, def) = market(request.market)?;
    let orders = load_orders(&id, now)?;
    let plan = plan(&request, &def, &orders, now)?;
    let parks = park_plan(&plan, &orders)?;
    Ok(TakeResult { plan, parks })
}

/// Park exactly what `plan` says against each order, and tell each maker.
/// The one path every take, market order and retry parks through.
fn park_plan(plan: &TakePlan<ActionHash>, orders: &[Order]) -> ExternResult<Vec<PlacedPark>> {
    let me = my_key()?;
    let mut parks = Vec::with_capacity(plan.fills.len());
    for fill in &plan.fills {
        let park: ActionHash = ledger(
            "park",
            ParkRequest {
                escrow: fill.order.clone(),
                amounts: fill.cost.clone(),
                requested_lots: fill.lots,
            },
        )?;
        if let Some(order) = orders.iter().find(|o| o.id == fill.order) {
            notify(
                DexSignal::ParkPlaced {
                    escrow: fill.order.clone(),
                    park: park.clone(),
                    taker: me.clone(),
                    lots: fill.lots,
                },
                vec![order.maker.clone()],
            )?;
        }
        parks.push(PlacedPark {
            escrow: fill.order.clone(),
            park,
            lots: fill.lots,
        });
    }
    Ok(parks)
}

// ---------------------------------------------------------------------------
// Market orders
// ---------------------------------------------------------------------------
//
// Taker-only and immediate-or-cancel: a sweep from the best price within a
// slippage limit, parked through `park_plan` like any take. Nothing rests on
// the book; each maker's run fills what it can at the maker's own price and
// refunds the rest. The slippage limit protects the taker's client only:
// validation does not know about it.

/// Read-only: what a market order would take right now.
#[hdk_extern]
pub fn preview_market_order(request: MarketPreviewRequest) -> ExternResult<MarketPlan<ActionHash>> {
    let now = now()?;
    let (id, def) = market(request.market)?;
    market_plan(&def, request.side, request.amount, request.max_slippage_bps, &load_orders(&id, now)?, now)
}

#[hdk_extern]
pub fn market_order(request: MarketOrderRequest) -> ExternResult<MarketResult> {
    execute_market(
        request.market,
        request.side,
        MarketAmount::Lots(request.lots),
        request.max_slippage_bps,
        request.expected.as_ref(),
    )
}

#[hdk_extern]
pub fn market_order_by_budget(request: MarketBudgetRequest) -> ExternResult<MarketResult> {
    execute_market(
        request.market,
        request.side,
        MarketAmount::Budget(request.budget),
        request.max_slippage_bps,
        request.expected.as_ref(),
    )
}

/// Retry a market order's unfilled lots once, against the fresh book but
/// within the ORIGINAL limit price, never one derived from the new best
/// price. Unfilled = the original shortfall plus each settled park's
/// unfilled lots; parks the makers have not run yet are not retried.
/// A retry cannot itself be retried: place a new market order instead.
#[hdk_extern]
pub fn retry_market_shortfall(request: RetryMarketRequest) -> ExternResult<MarketRetry> {
    let original = request.original;
    if original.attempt > 0 {
        return Err(guest("a market order's remainder may be retried once; place a new order instead"));
    }
    let statuses: Vec<ParkStatus> = ledger("get_my_parks", ())?;
    let mut unfilled = original.plan.plan.shortfall;
    let mut still_pending = 0;
    for placed in &original.parks {
        match statuses.iter().find(|s| s.park == placed.park).and_then(|s| s.settlement.as_ref()) {
            Some(settled) => unfilled += placed.lots.saturating_sub(settled.filled_lots),
            None => still_pending += 1,
        }
    }
    if unfilled == 0 {
        return Ok(MarketRetry {
            unfilled_lots: 0,
            still_pending,
            result: None,
        });
    }
    let now = now()?;
    let (id, def) = market(Some(original.market))?;
    let orders = load_orders(&id, now)?;
    let plan = book::market::plan_with_limit(
        &orders,
        &def,
        original.plan.take,
        unfilled,
        original.plan.limit_price,
        &my_key()?,
        now,
    )
    .map_err(market_err)?;
    let parks = park_plan(&plan.plan, &orders)?;
    Ok(MarketRetry {
        unfilled_lots: unfilled,
        still_pending,
        result: Some(MarketResult {
            market: id,
            plan,
            parks,
            changes: Vec::new(),
            attempt: original.attempt + 1,
        }),
    })
}

fn market_plan(
    market: &MarketDef,
    side: Side,
    amount: MarketAmount,
    max_slippage_bps: Option<u32>,
    orders: &[Order],
    now: i64,
) -> ExternResult<MarketPlan<ActionHash>> {
    let bps = max_slippage_bps.unwrap_or(DEFAULT_MAX_SLIPPAGE_BPS);
    let me = my_key()?;
    match amount {
        MarketAmount::Lots(lots) => book::plan_market(orders, market, side, lots, bps, &me, now),
        MarketAmount::Budget(budget) => book::plan_market_by_budget(orders, market, side, budget, bps, &me, now),
    }
    .map_err(market_err)
}

/// Plan from one fresh read and park in the same call, so nothing can change
/// between the two; `expected` (the confirmed preview) is what may differ.
fn execute_market(
    market_id: Option<MarketId>,
    side: Side,
    amount: MarketAmount,
    max_slippage_bps: Option<u32>,
    expected: Option<&MarketPlan<ActionHash>>,
) -> ExternResult<MarketResult> {
    let now = now()?;
    let (id, def) = market(market_id)?;
    let orders = load_orders(&id, now)?;
    let plan = market_plan(&def, side, amount, max_slippage_bps, &orders, now)?;
    let parks = park_plan(&plan.plan, &orders)?;
    let changes = expected.map(|e| fill_changes(&e.plan, &plan.plan)).unwrap_or_default();
    Ok(MarketResult {
        market: id,
        plan,
        parks,
        changes,
        attempt: 0,
    })
}

/// Orders whose lots differ between two plans, in the order they appear.
fn fill_changes(expected: &TakePlan<ActionHash>, planned: &TakePlan<ActionHash>) -> Vec<FillChange> {
    let lots_in = |plan: &TakePlan<ActionHash>, order: &ActionHash| {
        plan.fills.iter().find(|f| &f.order == order).map_or(0, |f| f.lots)
    };
    let mut seen = BTreeSet::new();
    expected
        .fills
        .iter()
        .chain(&planned.fills)
        .filter(|f| seen.insert(f.order.clone()))
        .filter_map(|f| {
            let (expected_lots, planned_lots) = (lots_in(expected, &f.order), lots_in(planned, &f.order));
            (expected_lots != planned_lots).then(|| FillChange {
                order: f.order.clone(),
                expected_lots,
                planned_lots,
            })
        })
        .collect()
}

fn market_err(e: book::MarketError) -> WasmError {
    guest(e.to_string())
}

// ---------------------------------------------------------------------------
// Maker settlement
// ---------------------------------------------------------------------------

/// Settle one of the caller's orders now: fill its pending parks.
#[hdk_extern]
pub fn settle_order(escrow: ActionHash) -> ExternResult<Vec<RunReport>> {
    run_until_settled(&escrow, RunMode::Fill)
}

/// Test-only: `settle_order`, returning how long each step took inside this
/// zome call, as `(step, microseconds)`. The caller's total minus their sum
/// is this call's own inline validation and flush (profiling, see
/// `docs/design/write-performance.md`).
#[hdk_extern]
pub fn settle_order_traced(escrow: ActionHash) -> ExternResult<Vec<(String, i64)>> {
    TRACE.with(|t| t.borrow_mut().clear());
    mark("start")?;
    run_until_settled(&escrow, RunMode::Fill)?;
    mark("end")?;
    let marks = TRACE.with(|t| t.borrow().clone());
    Ok(marks.windows(2).map(|w| (w[1].0.to_string(), w[1].1 - w[0].1)).collect())
}

thread_local! {
    /// Profiling marks for `settle_order_traced`; cleared at its start.
    static TRACE: std::cell::RefCell<Vec<(&'static str, i64)>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// Record a profiling mark (a `sys_time` call; negligible).
fn mark(step: &'static str) -> ExternResult<()> {
    let at = now()?;
    TRACE.with(|t| t.borrow_mut().push((step, at)));
    Ok(())
}

/// Cancel an order: release its lock and refund every pending taker.
#[hdk_extern]
pub fn cancel_order(escrow: ActionHash) -> ExternResult<Vec<RunReport>> {
    run_until_settled(&escrow, RunMode::Release)
}

/// Settle every order the caller made: fill pending parks on live orders;
/// release expired orders; refund parks that arrived after a release.
/// Idempotent.
///
/// A run can fail validation if its order expires between reading the clock
/// and committing (see `ledger::run_escrow`); that fails this whole call, and
/// calling again resolves it.
#[hdk_extern]
pub fn run_my_orders() -> ExternResult<Vec<RunReport>> {
    let now = now()?;
    // Read every order before any run writes: each ledger call made after a
    // write would re-validate this call's writes so far.
    let escrows: Vec<ActionHash> = ledger("get_my_escrows", ())?;
    let mut due = Vec::new();
    for escrow in escrows {
        let state: EscrowState = ledger("get_escrow_state", escrow.clone())?;
        let pending: Vec<PendingPark> = ledger("get_pending_parks", escrow.clone())?;
        let expired = state.terms.is_expired_at(now);
        let mode = if state.closed || expired {
            if state.closed && pending.is_empty() {
                continue;
            }
            RunMode::Release
        } else if pending.is_empty() {
            continue;
        } else {
            RunMode::Fill
        };
        due.push((escrow, mode, (pending, state)));
    }
    let mut reports = Vec::new();
    for (escrow, mode, read) in due {
        reports.extend(run_until_settled_from(&escrow, mode, Some(read))?);
    }
    Ok(reports)
}

/// Run `mode` until no park is left pending, at most `MAX_RUNS_PER_CALL` times.
/// Each run's receivers other than the caller are told to collect, with how
/// their own parks came out; the caller's UI is told the order's new status.
fn run_until_settled(escrow: &ActionHash, mode: RunMode) -> ExternResult<Vec<RunReport>> {
    run_until_settled_from(escrow, mode, None)
}

/// `run_until_settled`, using `read` (pending parks and state, read before
/// this call wrote anything) for the first run instead of reading again.
fn run_until_settled_from(
    escrow: &ActionHash,
    mode: RunMode,
    mut read: Option<(Vec<PendingPark>, EscrowState)>,
) -> ExternResult<Vec<RunReport>> {
    let me = my_key()?;
    let mut reports = Vec::new();
    for _ in 0..MAX_RUNS_PER_CALL {
        // Every ledger read happens before the run writes. A nested zome call
        // re-validates everything written earlier in this call (Holochain runs
        // inline validation at the end of each nested call too), so a read
        // after the write would validate the run and its links again.
        // Who parked what, read before the run consumes it.
        let (pending, before) = match read.take() {
            Some(read) => read,
            None => {
                let pending: Vec<PendingPark> = ledger("get_pending_parks", escrow.clone())?;
                mark("ledger.get_pending_parks")?;
                let before: EscrowState = ledger("get_escrow_state", escrow.clone())?;
                mark("ledger.get_escrow_state (before the run)")?;
                (pending, before)
            }
        };
        let report: Option<RunReport> = ledger(
            "run_escrow",
            RunEscrowInput {
                escrow: escrow.clone(),
                mode,
            },
        )?;
        mark("ledger.run_escrow (incl. its inline validation)")?;
        // `None`: a fill with nothing pending.
        let Some(report) = report else { break };
        for receiver in report.receivers.iter().filter(|r| **r != me) {
            let fills = report
                .fills
                .iter()
                .filter_map(|f| {
                    let p = pending.iter().find(|p| p.park == f.park && &p.taker == receiver)?;
                    Some(SignalFill { park: f.park.clone(), filled_lots: f.filled_lots, requested_lots: p.requested_lots })
                })
                .collect();
            notify(
                DexSignal::RunSettled {
                    escrow: escrow.clone(),
                    run: report.run.clone(),
                    maker: me.clone(),
                    mode,
                    fills,
                },
                vec![receiver.clone()],
            )?;
        }
        mark("signals")?;
        let run_ts = own_action_timestamp(&report.run)?;
        if report.mode == RunMode::Fill && report.filled_lots > 0 {
            index_trade(escrow, &report, &before, run_ts)?;
        }
        mark("index link")?;
        let (filled_lots, released_at) = after_run(&before, &report, run_ts)?;
        let status = book::order_status(before.terms.lots, filled_lots, before.terms.expires_at, released_at, now()?);
        if status != dex_api::OrderStatus::Open && status != dex_api::OrderStatus::Partial {
            unlist(escrow, &before)?;
        }
        mark("unlist")?;
        emit_signal(DexSignal::OrderUpdated {
            escrow: escrow.clone(),
            run: report.run.clone(),
            status,
            filled_lots,
            lots: before.terms.lots,
        })?;
        let done = report.still_pending == 0;
        reports.push(report);
        if done {
            break;
        }
    }
    Ok(reports)
}

/// The order's filled lots and release time after `report`'s run, from its
/// state before it, as `ledger.get_escrow_state` would report them: lots
/// still unfilled when the order is first released were returned to the
/// maker, not sold.
fn after_run(before: &EscrowState, report: &RunReport, run_ts: i64) -> ExternResult<(u64, Option<i64>)> {
    if let Some(released) = before.released_at {
        return Ok((before.filled_lots, Some(released.as_micros())));
    }
    match report.mode {
        RunMode::Fill => {
            let (_, def) = market(Some(before.market))?;
            let remaining = before.terms.remaining_lots(&report.locked, &def).map_err(core_err)?;
            Ok((before.terms.lots.saturating_sub(remaining), None))
        }
        RunMode::Release => Ok((before.terms.lots.saturating_sub(before.remaining_lots), Some(run_ts))),
    }
}

/// Index a run that sold lots under its market and day. Validation checks
/// the tag against the run, so readers can trust it without reading runs.
fn index_trade(escrow: &ActionHash, report: &RunReport, state: &EscrowState, run_ts: i64) -> ExternResult<()> {
    let tag = dex_core::buckets::TradeTag {
        price_per_lot: state.terms.price_per_lot,
        lots: report.filled_lots,
        maker_side: state.terms.side,
        run_ts,
        escrow: escrow.get_raw_39().to_vec(),
    };
    create_link(
        dex_integrity::trade_anchor(&state.market, utc_day(run_ts))?,
        report.run.clone(),
        LinkTypes::MarketTradesByDay,
        LinkTag::new(tag.encode()),
    )?;
    Ok(())
}

/// Delete a closed order's listings (an optimisation: readers still check
/// every listed order's state, so a listing that stays only costs a read).
///
/// The maker's own listing links are found on their own chain (link actions
/// after the escrow, headers only), so this never scans the day's listing
/// bucket, however many orders it holds.
fn unlist(escrow: &ActionHash, state: &EscrowState) -> ExternResult<()> {
    let anchor = AnyLinkableHash::from(listing_anchor(&state.market, utc_day(state.opened_at.as_micros()))?);
    let target = AnyLinkableHash::from(escrow.clone());
    let head = agent_info()?.chain_head.1;
    let since = own_action(escrow)?.action().action_seq();
    if since >= head {
        return Ok(());
    }
    let actions = query(
        ChainQueryFilter::new()
            .sequence_range(ChainQueryFilterRange::ActionSeqRange(since, head))
            .include_entries(false),
    )?;
    let mut listings = Vec::new();
    let mut deleted = BTreeSet::new();
    for record in &actions {
        match &record.action().data {
            ActionData::CreateLink(c) if c.base_address == anchor && c.target_address == target => {
                listings.push(record.action_address().clone());
            }
            ActionData::DeleteLink(d) => {
                deleted.insert(d.link_add_address.clone());
            }
            _ => {}
        }
    }
    for listing in listings.into_iter().filter(|l| !deleted.contains(l)) {
        delete_link(listing, GetOptions::local())?;
    }
    Ok(())
}

/// One of the caller's own actions (this call's included).
fn own_action(action: &ActionHash) -> ExternResult<Record> {
    query(ChainQueryFilter::new().sequence_range(ChainQueryFilterRange::ActionHashTerminated(action.clone(), 0)))?
        .into_iter()
        .next()
        .ok_or_else(|| guest("the action is not on the caller's chain"))
}

/// The timestamp of one of the caller's own actions (this call's included).
fn own_action_timestamp(action: &ActionHash) -> ExternResult<i64> {
    Ok(own_action(action)?.action().timestamp().as_micros())
}

// ---------------------------------------------------------------------------
// The caller's own view
// ---------------------------------------------------------------------------

/// The caller's orders, newest first, with pending parks and status.
#[hdk_extern]
pub fn my_orders() -> ExternResult<Vec<MyOrder>> {
    let now = now()?;
    let escrows: Vec<ActionHash> = ledger("get_my_escrows", ())?;
    let mut orders = Vec::with_capacity(escrows.len());
    for escrow in escrows {
        let state: EscrowState = ledger("get_escrow_state", escrow.clone())?;
        let pending: Vec<PendingPark> = ledger("get_pending_parks", escrow)?;
        let status = book::order_status(
            state.terms.lots,
            state.filled_lots,
            state.terms.expires_at,
            state.released_at.map(|t| t.as_micros()),
            now,
        );
        orders.push(MyOrder {
            state,
            pending,
            status,
        });
    }
    Ok(orders)
}

/// The caller's parks, newest first, with how each resolved.
#[hdk_extern]
pub fn my_parks() -> ExternResult<Vec<ParkStatus>> {
    ledger("get_my_parks", ())
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Call a `ledger` extern in this cell. Its commits join this call's workspace.
fn ledger<I, O>(fn_name: &str, input: I) -> ExternResult<O>
where
    I: Serialize + std::fmt::Debug,
    O: serde::de::DeserializeOwned + std::fmt::Debug,
{
    let response = call(
        CallTargetCell::Local,
        ZomeName::from(LEDGER),
        FunctionName::from(fn_name),
        None,
        input,
    )?;
    match response {
        ZomeCallResponse::Ok(io) => io
            .decode()
            .map_err(|e| wasm_error!(WasmErrorInner::Serialize(e))),
        ZomeCallResponse::AuthenticationFailed(_, _) => {
            Err(guest(format!("ledger.{fn_name}: authentication failed")))
        }
        ZomeCallResponse::Unauthorized(..) => Err(guest(format!("ledger.{fn_name}: unauthorized"))),
        ZomeCallResponse::NetworkError(e) => Err(guest(format!("ledger.{fn_name}: network error: {e}"))),
        ZomeCallResponse::CountersigningSession(e) => {
            Err(guest(format!("ledger.{fn_name}: countersigning session: {e}")))
        }
    }
}

fn my_key() -> ExternResult<AgentPubKey> {
    Ok(agent_info()?.agent_initial_pubkey)
}

fn now() -> ExternResult<i64> {
    Ok(sys_time()?.as_micros())
}

fn guest(message: impl Into<String>) -> WasmError {
    wasm_error!(WasmErrorInner::Guest(message.into()))
}

fn core_err(e: dex_core::CoreError) -> WasmError {
    guest(e.to_string())
}
