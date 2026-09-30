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
    BookView, DexSignal, FillChange, LevelQuery, MarketAmount, MarketBudgetRequest, MarketOrderRequest,
    MarketPlan, MarketPreviewRequest, MarketResult, MarketRetry, MyOrder, Order, PlacedPark,
    RawListing, RetryMarketRequest, TakePlan, TakeRequest, TakeResult, DEFAULT_MAX_SLIPPAGE_BPS,
};
use dex_core::book;
use dex_core::listing::{decode_tag, encode_tag};
use dex_core::properties::DexProperties;
use dex_core::{MarketDef, MarketId};
use dex_integrity::{load_properties, market_anchor, LinkTypes};
use hdk::prelude::*;
use ledger_api::{
    EscrowState, OpenEscrowRequest, OrderTerms, ParkRequest, ParkStatus, PendingPark, RunEscrowInput, RunMode, Side,
    RunReport,
};
use std::collections::BTreeSet;

const LEDGER: &str = "ledger";

/// Most settlement runs one call makes on one escrow. Each run consumes up to
/// `dex_core::MAX_PARKS_PER_RUN` parks; anything beyond waits for the next call.
const MAX_RUNS_PER_CALL: usize = 10;

// ---------------------------------------------------------------------------
// Signals
// ---------------------------------------------------------------------------

/// Let other agents send this cell remote signals, and nothing else.
#[hdk_extern]
pub fn init() -> ExternResult<InitCallbackResult> {
    let mut functions = HashSet::new();
    functions.insert((zome_info()?.name, FunctionName::from("recv_remote_signal")));
    create_cap_grant(CapGrantEntry {
        tag: "remote_signals".into(),
        access: CapAccess::Unrestricted,
        functions: GrantedFunctions::Listed(functions),
    })?;
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
// Listing
// ---------------------------------------------------------------------------

/// Open an order's escrow and list it in the book. Returns the escrow hash,
/// which is the order's identity.
#[hdk_extern]
pub fn place_order(terms: OrderTerms) -> ExternResult<ActionHash> {
    let (market, _) = default_market()?;
    let escrow: ActionHash = ledger("open_escrow", OpenEscrowRequest { market, terms })?;
    list(escrow.clone(), &market, encode_tag(&market, &terms))?;
    Ok(escrow)
}

/// List an escrow the caller opened through the ledger directly. Validation
/// rejects it unless the caller is the escrow's maker.
#[hdk_extern]
pub fn republish_listing(escrow: ActionHash) -> ExternResult<ActionHash> {
    let state: EscrowState = ledger("get_escrow_state", escrow.clone())?;
    list(escrow, &state.market, encode_tag(&state.market, &state.terms))
}

/// List with a caller-chosen tag. Exists to test that validation rejects any
/// tag except the escrow's own; it cannot create a listing `place_order` could not.
#[hdk_extern]
pub fn list_escrow_raw(input: RawListing) -> ExternResult<ActionHash> {
    let market = match input.anchor_market {
        Some(market) => market,
        None => ledger::<_, EscrowState>("get_escrow_state", input.escrow.clone())?.market,
    };
    list(input.escrow, &market, input.tag)
}

fn list(escrow: ActionHash, market: &MarketId, tag: Vec<u8>) -> ExternResult<ActionHash> {
    create_link(market_anchor(market)?, escrow, LinkTypes::MarketToOrders, LinkTag::new(tag))
}

/// The DNA properties; unusable properties are an error, never a default.
fn props() -> ExternResult<DexProperties> {
    load_properties()?.map_err(guest)
}

/// The market this zome trades in: the first the DNA properties declare. The
/// UI's market selector (milestone 2 step 4) will pass one explicitly.
fn default_market() -> ExternResult<(MarketId, MarketDef)> {
    let props = props()?;
    let market = props
        .markets
        .first()
        .cloned()
        .ok_or_else(|| guest("the DNA properties declare no market"))?;
    Ok((market.id(), market))
}

// ---------------------------------------------------------------------------
// Book reads
// ---------------------------------------------------------------------------

#[hdk_extern]
pub fn get_order_book() -> ExternResult<BookView> {
    let now = now()?;
    Ok(book::aggregate(&load_orders(now)?, now))
}

/// The orders at one price level, in time priority.
#[hdk_extern]
pub fn get_level_orders(query: LevelQuery) -> ExternResult<Vec<Order>> {
    let now = now()?;
    Ok(book::orders_at_level(&load_orders(now)?, query.side, query.price_per_lot, now))
}

/// Preview a take. Read-only.
#[hdk_extern]
pub fn plan_take(request: TakeRequest) -> ExternResult<TakePlan<ActionHash>> {
    let now = now()?;
    plan(&request, &load_orders(now)?, now)
}

fn plan(request: &TakeRequest, orders: &[Order], now: i64) -> ExternResult<TakePlan<ActionHash>> {
    book::plan_take(orders, &default_market()?.1, &my_key()?, request.take, request.lots, request.limit_price, now)
        .map_err(core_err)
}

/// Every live listed order.
///
/// Cost: one `get_links`, then one ledger state read per listing that is not
/// already expired by its tag (N+1). Fine at MVP scale; a cache or a
/// maker-published summary would go here. Orders whose state cannot be read
/// yet (not gossiped) are left out rather than failing the whole book.
fn load_orders(now: i64) -> ExternResult<Vec<Order>> {
    let (market, _) = default_market()?;
    let links = get_links(
        LinkQuery::try_new(market_anchor(&market)?, LinkTypes::MarketToOrders)?,
        GetStrategy::Network,
    )?;
    let mut seen = BTreeSet::new();
    let mut orders = Vec::new();
    for link in links {
        let Some(escrow) = link.target.into_action_hash() else {
            continue;
        };
        if !seen.insert(escrow.clone()) {
            continue; // the same escrow listed twice
        }
        match decode_tag(&link.tag.0) {
            Some(tag) if tag.market == market && now < tag.expires_at => {}
            _ => continue,
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
    let orders = load_orders(now)?;
    let plan = plan(&request, &orders, now)?;
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
    market_plan(request.side, request.amount, request.max_slippage_bps, &load_orders(now)?, now)
}

#[hdk_extern]
pub fn market_order(request: MarketOrderRequest) -> ExternResult<MarketResult> {
    execute_market(
        request.side,
        MarketAmount::Lots(request.lots),
        request.max_slippage_bps,
        request.expected.as_ref(),
    )
}

#[hdk_extern]
pub fn market_order_by_budget(request: MarketBudgetRequest) -> ExternResult<MarketResult> {
    execute_market(
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
    let orders = load_orders(now)?;
    let plan = book::market::plan_with_limit(
        &orders,
        &default_market()?.1,
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
            plan,
            parks,
            changes: Vec::new(),
            attempt: original.attempt + 1,
        }),
    })
}

fn market_plan(
    side: Side,
    amount: MarketAmount,
    max_slippage_bps: Option<u32>,
    orders: &[Order],
    now: i64,
) -> ExternResult<MarketPlan<ActionHash>> {
    let bps = max_slippage_bps.unwrap_or(DEFAULT_MAX_SLIPPAGE_BPS);
    let me = my_key()?;
    let (_, market) = default_market()?;
    match amount {
        MarketAmount::Lots(lots) => book::plan_market(orders, &market, side, lots, bps, &me, now),
        MarketAmount::Budget(budget) => book::plan_market_by_budget(orders, &market, side, budget, bps, &me, now),
    }
    .map_err(market_err)
}

/// Plan from one fresh read and park in the same call, so nothing can change
/// between the two; `expected` (the confirmed preview) is what may differ.
fn execute_market(
    side: Side,
    amount: MarketAmount,
    max_slippage_bps: Option<u32>,
    expected: Option<&MarketPlan<ActionHash>>,
) -> ExternResult<MarketResult> {
    let now = now()?;
    let orders = load_orders(now)?;
    let plan = market_plan(side, amount, max_slippage_bps, &orders, now)?;
    let parks = park_plan(&plan.plan, &orders)?;
    let changes = expected.map(|e| fill_changes(&e.plan, &plan.plan)).unwrap_or_default();
    Ok(MarketResult {
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
    let mut reports = Vec::new();
    let escrows: Vec<ActionHash> = ledger("get_my_escrows", ())?;
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
        reports.extend(run_until_settled(&escrow, mode)?);
    }
    Ok(reports)
}

/// Run `mode` until no park is left pending, at most `MAX_RUNS_PER_CALL` times.
/// Each run's receivers other than the caller are told to collect.
fn run_until_settled(escrow: &ActionHash, mode: RunMode) -> ExternResult<Vec<RunReport>> {
    let me = my_key()?;
    let mut reports = Vec::new();
    for _ in 0..MAX_RUNS_PER_CALL {
        let report: Option<RunReport> = ledger(
            "run_escrow",
            RunEscrowInput {
                escrow: escrow.clone(),
                mode,
            },
        )?;
        // `None`: a fill with nothing pending.
        let Some(report) = report else { break };
        notify(
            DexSignal::RunSettled {
                escrow: escrow.clone(),
                run: report.run.clone(),
                maker: me.clone(),
                mode,
            },
            report.receivers.iter().filter(|r| **r != me).cloned().collect(),
        )?;
        let done = report.still_pending == 0;
        reports.push(report);
        if done {
            break;
        }
    }
    Ok(reports)
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
