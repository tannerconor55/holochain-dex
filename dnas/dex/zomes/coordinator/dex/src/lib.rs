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
    BookView, LevelQuery, MyOrder, Order, PlacedPark, RawListing, TakePlan, TakeRequest,
    TakeResult,
};
use dex_core::book;
use dex_core::listing::{decode_tag, encode_tag};
use dex_integrity::{market_anchor, LinkTypes};
use hdk::prelude::*;
use ledger_api::{
    EscrowState, OrderTerms, ParkRequest, ParkStatus, PendingPark, RunEscrowInput, RunMode,
    RunReport,
};
use std::collections::BTreeSet;

const LEDGER: &str = "ledger";

/// Most settlement runs one call makes on one escrow. Each run consumes up to
/// `dex_core::MAX_PARKS_PER_RUN` parks; anything beyond waits for the next call.
const MAX_RUNS_PER_CALL: usize = 10;

// ---------------------------------------------------------------------------
// Listing
// ---------------------------------------------------------------------------

/// Open an order's escrow and list it in the book. Returns the escrow hash,
/// which is the order's identity.
#[hdk_extern]
pub fn place_order(terms: OrderTerms) -> ExternResult<ActionHash> {
    let escrow: ActionHash = ledger("open_escrow", terms)?;
    list(escrow.clone(), encode_tag(&terms))?;
    Ok(escrow)
}

/// List an escrow the caller opened through the ledger directly. Validation
/// rejects it unless the caller is the escrow's maker.
#[hdk_extern]
pub fn republish_listing(escrow: ActionHash) -> ExternResult<ActionHash> {
    let state: EscrowState = ledger("get_escrow_state", escrow.clone())?;
    list(escrow, encode_tag(&state.terms))
}

/// List with a caller-chosen tag. Exists to test that validation rejects any
/// tag except the escrow's own; it cannot create a listing `place_order` could not.
#[hdk_extern]
pub fn list_escrow_raw(input: RawListing) -> ExternResult<ActionHash> {
    list(input.escrow, input.tag)
}

fn list(escrow: ActionHash, tag: Vec<u8>) -> ExternResult<ActionHash> {
    create_link(market_anchor()?, escrow, LinkTypes::MarketToOrders, LinkTag::new(tag))
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
    book::plan_take(orders, &my_key()?, request.take, request.lots, request.limit_price, now)
        .map_err(core_err)
}

/// Every live listed order.
///
/// Cost: one `get_links`, then one ledger state read per listing that is not
/// already expired by its tag (N+1). Fine at MVP scale; a cache or a
/// maker-published summary would go here. Orders whose state cannot be read
/// yet (not gossiped) are left out rather than failing the whole book.
fn load_orders(now: i64) -> ExternResult<Vec<Order>> {
    let links = get_links(
        LinkQuery::try_new(market_anchor()?, LinkTypes::MarketToOrders)?,
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
            Some(tag) if now < tag.expires_at => {}
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
    let plan = plan(&request, &load_orders(now)?, now)?;
    let mut parks = Vec::with_capacity(plan.fills.len());
    for fill in &plan.fills {
        let park: ActionHash = ledger(
            "park",
            ParkRequest {
                escrow: fill.order.clone(),
                amounts: fill.cost,
                requested_lots: fill.lots,
            },
        )?;
        // Phase 4: signal the maker here.
        parks.push(PlacedPark {
            escrow: fill.order.clone(),
            park,
            lots: fill.lots,
        });
    }
    Ok(TakeResult { plan, parks })
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
fn run_until_settled(escrow: &ActionHash, mode: RunMode) -> ExternResult<Vec<RunReport>> {
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
