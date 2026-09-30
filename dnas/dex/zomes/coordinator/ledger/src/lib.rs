//! # ledger — mock Unyt settlement interface
//!
//! These zome functions are the *only* way the DEX touches money. Replacing
//! this mock with Unyt later means reimplementing this small surface against
//! Unyt's Smart Agreements; nothing above it should change.
//!
//! | Function            | Unyt equivalent (planned)                              |
//! |---------------------|--------------------------------------------------------|
//! | `mint`              | test-network issuance                                  |
//! | `open_escrow`       | create the order's Smart Agreement + park maker funds  |
//! | `park`              | taker parks a spend + request data                     |
//! | `run_escrow`        | maker executes the agreement (a RAVE)                  |
//! | `collect_all`       | receivers collect allocations                          |
//! | `reclaim_park`      | taker withdraws a parked spend never consumed          |
//! | `get_escrow_trades` | an order's trades, derived from its runs (read)        |
//!
//! Reads use `GetStrategy::Network` throughout: v1 targets desktop full-arc
//! nodes only.

use dex_core::checkpoint::{CheckpointState, CHECKPOINT_AFTER_ACTIONS, CHECKPOINT_EVERY};
use dex_core::properties::{DexProperties, Timing};
use dex_core::timeout::park_deadline;
use dex_core::{execute_run, select_parks, ParkInput, RunInput};
use hdk::prelude::*;
use ledger_api::{
    BalanceView, CheckpointView, EscrowState, OpenEscrowRequest, ParkFill, ParkRequest, ParkSettlement,
    ParkStatus, PendingPark, RawCheckpoint, RawPark, RawReclaim, RunEscrowInput, RunReport,
};
use ledger_integrity::*;
use std::collections::BTreeSet;

// ---------------------------------------------------------------------------
// Writes
// ---------------------------------------------------------------------------

/// Test faucet. MVP only.
#[hdk_extern]
pub fn mint(amounts: Amounts) -> ExternResult<ActionHash> {
    let hash = create_entry(&EntryTypes::Mint(Mint { amounts }))?;
    checkpoint_if_due()?;
    Ok(hash)
}

/// Open an order's escrow in a declared market, locking the maker's funds.
/// The returned hash is the order's identity.
#[hdk_extern]
pub fn open_escrow(request: OpenEscrowRequest) -> ExternResult<ActionHash> {
    let props = props()?;
    props.market(&request.market).map_err(|e| guest(e.to_string()))?;
    let escrow = create_entry(&EntryTypes::Escrow(Escrow {
        market: request.market,
        terms: request.terms,
        checkpoint: checkpoint_if_due()?,
    }))?;
    create_link(my_key()?, escrow.clone(), LinkTypes::AgentToEscrows, ())?;
    Ok(escrow)
}

/// Park taker funds against a live order. Unused funds are refunded by the
/// maker's next run.
#[hdk_extern]
pub fn park(request: ParkRequest) -> ExternResult<ActionHash> {
    let (_, escrow) = get_escrow(&request.escrow)?;
    let park = create_entry(&EntryTypes::Park(Park {
        escrow: request.escrow.clone(),
        market: escrow.market,
        amounts: request.amounts,
        requested_lots: request.requested_lots,
        checkpoint: checkpoint_if_due()?,
    }))?;
    create_link(request.escrow, park.clone(), LinkTypes::EscrowToParks, ())?;
    Ok(park)
}

/// Execute one settlement run on an escrow the caller made.
///
/// `Fill` consumes up to `MAX_PARKS_PER_RUN` pending parks in time order and
/// returns `None` if there is nothing to fill. `Release` always runs: it refunds
/// pending parks and returns the remaining lock to the maker. Call `Release`
/// repeatedly until `still_pending` is zero to refund every waiting taker.
///
/// The run's validation uses its own action timestamp. If an order expires in
/// the moment between this call reading the clock and the commit, the commit
/// fails validation; calling again resolves it.
#[hdk_extern]
pub fn run_escrow(input: RunEscrowInput) -> ExternResult<Option<RunReport>> {
    let me = my_key()?;
    let (escrow_record, escrow) = get_escrow(&input.escrow)?;
    if escrow_record.action().author() != &me {
        return Err(guest("only the escrow's maker can run it"));
    }

    let props = props()?;
    let market = market_of(&props, &escrow)?.clone();
    let timing = props.timing().map_err(|e| guest(e.to_string()))?;
    let now = sys_time()?.as_micros();

    // Our own chain is authoritative for our runs: read it locally.
    let mine = own_chain()?;
    let mut my_runs: Vec<&(ActionHash, u32, SettlementRun)> = mine
        .runs
        .iter()
        .filter(|(_, _, r)| r.escrow == input.escrow)
        .collect();
    my_runs.sort_by_key(|(_, seq, _)| std::cmp::Reverse(*seq));
    let prev_run = my_runs.first().map(|(hash, _, _)| hash.clone());
    let prev_locked = match my_runs.first() {
        Some((_, _, run)) => run.locked.clone(),
        None => escrow.terms.initial_lock(&market).map_err(core_err)?,
    };
    let consumed: BTreeSet<ActionHash> = my_runs
        .iter()
        .flat_map(|(_, _, r)| r.consumed.iter().cloned())
        .collect();

    // Parks past (or within a margin of) their deadline are the taker's to
    // reclaim; consuming one would fail validation.
    let pending: Vec<_> = pending_park_inputs(&input.escrow, &consumed)?
        .into_iter()
        .filter(|p| {
            park_deadline(p.parked_at, escrow.terms.expires_at, &timing)
                .is_some_and(|deadline| now.saturating_add(deadline_margin(&timing)) < deadline)
        })
        .collect();
    let pending_count = pending.len();
    let parks = select_parks(pending);
    if input.mode == RunMode::Fill && parks.is_empty() {
        return Ok(None);
    }

    let run_input = RunInput {
        terms: escrow.terms,
        market,
        maker: me,
        prev_locked,
        parks,
        now,
        mode: input.mode,
    };
    let output = execute_run(&run_input).map_err(core_err)?;

    let run = SettlementRun {
        escrow: input.escrow.clone(),
        prev_run,
        mode: input.mode,
        consumed: output.consumed.clone(),
        allocations: output.allocations.clone(),
        locked: output.locked.clone(),
    };
    let run_hash = create_entry(&EntryTypes::SettlementRun(run))?;
    create_link(input.escrow, run_hash.clone(), LinkTypes::EscrowToRuns, ())?;
    for allocation in &output.allocations {
        create_link(
            allocation.receiver.clone(),
            run_hash.clone(),
            LinkTypes::AgentToIncomingRuns,
            (),
        )?;
    }
    checkpoint_if_due()?;

    Ok(Some(RunReport {
        run: run_hash,
        mode: input.mode,
        filled_lots: output.filled_lots,
        fills: output
            .outcomes
            .into_iter()
            .map(|o| ParkFill {
                park: o.id,
                filled_lots: o.filled_lots,
            })
            .collect(),
        locked: output.locked,
        still_pending: pending_count - run_input.parks.len(),
        receivers: output.allocations.iter().map(|a| a.receiver.clone()).collect(),
    }))
}

/// Collect every allocation paid to the caller that isn't collected yet.
#[hdk_extern]
pub fn collect_all() -> ExternResult<Vec<ActionHash>> {
    let me = my_key()?;
    let collected = collected_set(&own_chain()?);
    let mut created = Vec::new();
    for (run_hash, run) in incoming_runs(&me)? {
        for (index, allocation) in run.allocations.iter().enumerate() {
            let index = index as u32;
            if allocation.receiver != me || collected.contains(&(run_hash.clone(), index)) {
                continue;
            }
            created.push(create_entry(&EntryTypes::Collect(Collect {
                run: run_hash.clone(),
                index,
                amounts: allocation.amounts.clone(),
                // Rechecked per collect: one call can write many.
                checkpoint: checkpoint_if_due()?,
            }))?);
        }
    }
    Ok(created)
}

/// Take back a park its maker never settled.
///
/// Possible once the park's deadline has passed **and** the maker has written
/// any action at or after it: that action is the anchor proving no run can
/// still consume the park (design doc section 4.2). A maker who never writes
/// again cannot be reclaimed against; the error says so.
#[hdk_extern]
pub fn reclaim_park(park: ActionHash) -> ExternResult<ActionHash> {
    let me = my_key()?;
    let props = props()?;
    let timing = props.timing().map_err(|e| guest(e.to_string()))?;
    let park_record = get_record(&park)?;
    let Some(EntryTypes::Park(parked)) = decode_record(&park_record)? else {
        return Err(guest(format!("{park} is not a park")));
    };
    if park_record.action().author() != &me {
        return Err(guest("only the park's author can reclaim it"));
    }
    if own_chain()?.reclaims.iter().any(|r| r.park == park) {
        return Err(guest("this park was already reclaimed"));
    }
    if escrow_runs(&parked.escrow)?.iter().any(|r| r.run.consumed.contains(&park)) {
        return Err(guest("this park was settled by the maker"));
    }
    let (escrow_record, escrow) = get_escrow(&parked.escrow)?;
    let deadline = park_deadline(park_record.action().timestamp().as_micros(), escrow.terms.expires_at, &timing)
        .ok_or_else(|| guest("park deadline overflows"))?;
    let anchor = maker_anchor(escrow_record.action().author(), deadline)?.ok_or_else(|| {
        guest("not reclaimable yet: the maker has written nothing since the park's deadline")
    })?;
    let hash = create_entry(&EntryTypes::Reclaim(Reclaim { park, anchor }))?;
    create_link(parked.escrow, hash.clone(), LinkTypes::EscrowToReclaims, ())?;
    checkpoint_if_due()?;
    Ok(hash)
}

// ---------------------------------------------------------------------------
// Test-only writes
// ---------------------------------------------------------------------------
//
// Each writes one entry exactly as given, skipping the checks above, so a
// test can show that validation (not this coordinator) refuses it. They can
// write nothing validation accepts that the externs above could not.

/// A park with a caller-chosen market and checkpoint citation, linked from
/// its escrow like any park so the maker sees it.
#[hdk_extern]
pub fn park_raw(raw: RawPark) -> ExternResult<ActionHash> {
    let park = create_entry(&EntryTypes::Park(Park {
        escrow: raw.escrow.clone(),
        market: raw.market,
        amounts: raw.amounts,
        requested_lots: raw.requested_lots,
        checkpoint: raw.checkpoint,
    }))?;
    create_link(raw.escrow, park.clone(), LinkTypes::EscrowToParks, ())?;
    Ok(park)
}

/// A reclaim citing any anchor.
#[hdk_extern]
pub fn reclaim_raw(raw: RawReclaim) -> ExternResult<ActionHash> {
    create_entry(&EntryTypes::Reclaim(Reclaim { park: raw.park, anchor: raw.anchor }))
}

/// A checkpoint with any contents.
#[hdk_extern]
pub fn checkpoint_raw(raw: RawCheckpoint) -> ExternResult<ActionHash> {
    create_entry(&EntryTypes::Checkpoint(Checkpoint { prev: raw.prev, state: raw.state }))
}

// ---------------------------------------------------------------------------
// Reads
// ---------------------------------------------------------------------------

/// An escrow's trades, oldest first: one per `Fill` run that sold lots,
/// derived by `dex_trades` from the runs (nothing is stored).
///
/// Cost: one escrow read, one `get_links`, one read per run.
#[hdk_extern]
pub fn get_escrow_trades(escrow: ActionHash) -> ExternResult<Vec<dex_trades::Trade<ActionHash>>> {
    let (_, entry) = get_escrow(&escrow)?;
    let props = props()?;
    let market = market_of(&props, &entry)?;
    let runs: Vec<dex_trades::RunRecord<ActionHash>> = escrow_runs(&escrow)?
        .into_iter()
        .map(|r| dex_trades::RunRecord {
            run: r.hash,
            timestamp: r.at.as_micros(),
            mode: r.run.mode,
            locked: r.run.locked,
        })
        .collect();
    dex_trades::trades_of_order(&escrow, market, &entry.terms, &runs).map_err(|e| guest(e.to_string()))
}

/// The caller's checkpoints, oldest first.
#[hdk_extern]
pub fn get_my_checkpoints() -> ExternResult<Vec<CheckpointView>> {
    let mut out = Vec::new();
    for record in query(ChainQueryFilter::new().include_entries(true))? {
        if let Some(EntryTypes::Checkpoint(c)) = decode_record(&record)? {
            out.push(CheckpointView { checkpoint: record.action_address().clone(), state: c.state });
        }
    }
    Ok(out)
}

#[hdk_extern]
pub fn get_escrow_state(escrow: ActionHash) -> ExternResult<EscrowState> {
    let (record, entry) = get_escrow(&escrow)?;
    let maker = record.action().author().clone();
    let runs = escrow_runs(&escrow)?;
    let props = props()?;
    let market = market_of(&props, &entry)?;

    // Walk the runs in order. Lots still unfilled when the order was released
    // were returned to the maker, not sold, so they don't count as filled.
    let mut locked = entry.terms.initial_lock(market).map_err(core_err)?;
    let mut unfilled_at_release = None;
    let mut released_at = None;
    for r in &runs {
        if r.run.mode == RunMode::Release && unfilled_at_release.is_none() {
            unfilled_at_release = Some(entry.terms.remaining_lots(&locked, market).map_err(core_err)?);
            released_at = Some(r.at);
        }
        locked = r.run.locked.clone();
    }
    let remaining_lots = entry.terms.remaining_lots(&locked, market).map_err(core_err)?;
    let unfilled = unfilled_at_release.unwrap_or(remaining_lots);
    let released = unfilled_at_release.is_some();
    Ok(EscrowState {
        escrow,
        maker,
        market: entry.market,
        terms: entry.terms,
        opened_at: record.action().timestamp(),
        locked,
        remaining_lots,
        filled_lots: entry.terms.lots.saturating_sub(unfilled),
        runs: runs.len(),
        expired: entry.terms.is_expired_at(sys_time()?.as_micros()),
        closed: released,
        released_at,
    })
}

/// Parks against an escrow that no run has consumed yet, oldest first.
#[hdk_extern]
pub fn get_pending_parks(escrow: ActionHash) -> ExternResult<Vec<PendingPark>> {
    let consumed: BTreeSet<ActionHash> = escrow_runs(&escrow)?
        .into_iter()
        .flat_map(|r| r.run.consumed)
        .collect();
    let mut parks = pending_park_inputs(&escrow, &consumed)?;
    parks.sort_by(|x, y| (x.parked_at, &x.id).cmp(&(y.parked_at, &y.id)));
    Ok(parks
        .into_iter()
        .map(|p| PendingPark {
            park: p.id,
            taker: p.taker,
            amounts: p.amounts,
            requested_lots: p.requested_lots,
            parked_at: Timestamp::from_micros(p.parked_at),
        })
        .collect())
}

/// Parks the caller has placed, newest first, with how each resolved.
#[hdk_extern]
pub fn get_my_parks() -> ExternResult<Vec<ParkStatus>> {
    let props = props()?;
    let timing = props.timing().map_err(|e| guest(e.to_string()))?;
    let now = sys_time()?.as_micros();
    let own = own_chain()?;
    let mut out = Vec::new();
    for record in query(ChainQueryFilter::new().include_entries(true))?.into_iter().rev() {
        let Some(EntryTypes::Park(park)) = decode_record(&record)? else {
            continue;
        };
        let hash = record.action_address().clone();
        let runs = escrow_runs(&park.escrow)?;
        let settlement = match runs.iter().find(|r| r.run.consumed.contains(&hash)) {
            Some(consuming) => Some(ParkSettlement {
                run: consuming.hash.clone(),
                filled_lots: replay_fill(&park.escrow, &runs, consuming, &hash)?,
            }),
            None => None,
        };
        let (escrow_record, escrow) = get_escrow(&park.escrow)?;
        let deadline = park_deadline(record.action().timestamp().as_micros(), escrow.terms.expires_at, &timing)
            .ok_or_else(|| guest("park deadline overflows"))?;
        let reclaimed = own.reclaims_by_park.get(&hash).cloned();
        let reclaimable = settlement.is_none()
            && reclaimed.is_none()
            && now >= deadline
            && maker_anchor(escrow_record.action().author(), deadline)?.is_some();
        out.push(ParkStatus {
            park: hash,
            escrow: park.escrow,
            amounts: park.amounts,
            requested_lots: park.requested_lots,
            parked_at: record.action().timestamp(),
            settlement,
            deadline: Timestamp::from_micros(deadline),
            reclaimed,
            reclaimable,
        });
    }
    Ok(out)
}

/// Escrows the caller has opened, newest first.
#[hdk_extern]
pub fn get_my_escrows() -> ExternResult<Vec<ActionHash>> {
    let mut escrows = own_chain()?.escrows;
    escrows.reverse();
    Ok(escrows.into_iter().map(|(hash, _)| hash).collect())
}

#[hdk_extern]
pub fn get_balance() -> ExternResult<BalanceView> {
    let me = my_key()?;
    let props = props()?;
    let mine = own_chain()?;
    let available = own_state(&mine, &props)?
        .totals
        .available()
        .map_err(|e| guest(e.to_string()))?;

    let mut locked_in_escrows = Amounts::ZERO;
    for (escrow_hash, escrow) in &mine.escrows {
        let latest = mine
            .runs
            .iter()
            .filter(|(_, _, r)| &r.escrow == escrow_hash)
            .max_by_key(|(_, seq, _)| *seq);
        let lock = match latest {
            Some((_, _, run)) => run.locked.clone(),
            None => escrow.terms.initial_lock(market_of(&props, escrow)?).map_err(core_err)?,
        };
        locked_in_escrows = add(&locked_in_escrows, &lock)?;
    }

    let mut parked = Amounts::ZERO;
    let reclaimed: BTreeSet<&ActionHash> = mine.reclaims.iter().map(|r| &r.park).collect();
    for (park_hash, park) in &mine.parks {
        let consumed = escrow_runs(&park.escrow)?
            .iter()
            .any(|r| r.run.consumed.contains(park_hash));
        if !consumed && !reclaimed.contains(park_hash) {
            parked = add(&parked, &park.amounts)?;
        }
    }

    let collected = collected_set(&mine);
    let mut uncollected = Amounts::ZERO;
    for (run_hash, run) in incoming_runs(&me)? {
        for (index, allocation) in run.allocations.iter().enumerate() {
            if allocation.receiver == me && !collected.contains(&(run_hash.clone(), index as u32)) {
                uncollected = add(&uncollected, &allocation.amounts)?;
            }
        }
    }

    let total = add(&add(&add(&available, &locked_in_escrows)?, &parked)?, &uncollected)?;
    Ok(BalanceView {
        available,
        locked_in_escrows,
        parked,
        uncollected,
        total,
    })
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn my_key() -> ExternResult<AgentPubKey> {
    Ok(agent_info()?.agent_initial_pubkey)
}

fn guest(message: impl Into<String>) -> WasmError {
    wasm_error!(WasmErrorInner::Guest(message.into()))
}

fn core_err(e: dex_core::CoreError) -> WasmError {
    guest(e.to_string())
}

fn add(x: &Amounts, y: &Amounts) -> ExternResult<Amounts> {
    x.checked_add(y).ok_or_else(|| guest("amount overflow"))
}

fn get_record(hash: &ActionHash) -> ExternResult<Record> {
    get(hash.clone(), GetOptions::network())?
        .ok_or_else(|| guest(format!("record {hash} not found")))
}

fn get_escrow(hash: &ActionHash) -> ExternResult<(Record, Escrow)> {
    let record = get_record(hash)?;
    match decode_record(&record)? {
        Some(EntryTypes::Escrow(escrow)) => Ok((record, escrow)),
        _ => Err(guest(format!("{hash} is not an escrow"))),
    }
}

/// Parks within this long of their deadline are left for the taker to
/// reclaim rather than consumed, so a run never races its own validation.
const DEADLINE_MARGIN_US: i64 = 60_000_000;

/// `DEADLINE_MARGIN_US`, but at most a fifth of the park's whole window, so a
/// short window (a test DNA's 25 s) still leaves the maker most of it.
fn deadline_margin(timing: &Timing) -> i64 {
    DEADLINE_MARGIN_US.min(timing.park_timeout_us.saturating_add(timing.settle_grace_us) / 5)
}

/// The DNA properties; unusable properties are an error, never a default.
fn props() -> ExternResult<DexProperties> {
    load_properties()?.map_err(guest)
}

fn market_of<'p>(props: &'p DexProperties, escrow: &Escrow) -> ExternResult<&'p MarketDef> {
    props.market(&escrow.market).map_err(|e| guest(e.to_string()))
}

/// The caller's ledger entries, read from their own source chain.
#[derive(Default)]
struct OwnChain {
    escrows: Vec<(ActionHash, Escrow)>,
    parks: Vec<(ActionHash, Park)>,
    /// `(action hash, action_seq, run)`.
    runs: Vec<(ActionHash, u32, SettlementRun)>,
    collects: Vec<Collect>,
    reclaims: Vec<Reclaim>,
    reclaims_by_park: std::collections::BTreeMap<ActionHash, ActionHash>,
    latest_checkpoint: Option<ActionHash>,
    /// The latest checkpoint's `action_seq`.
    checkpoint_seq: Option<u32>,
    /// The chain head's `action_seq`.
    head_seq: u32,
    /// Ledger entries after the latest checkpoint.
    ledger_since_checkpoint: usize,
    /// Every ledger entry, oldest first.
    ledger: Vec<(ActionHash, EntryTypes)>,
}

fn own_chain() -> ExternResult<OwnChain> {
    let mut own = OwnChain::default();
    for record in query(ChainQueryFilter::new().include_entries(true))? {
        own.head_seq = own.head_seq.max(record.action().action_seq());
        let Some(entry) = decode_record(&record)? else { continue };
        let hash = record.action_address().clone();
        own.ledger_since_checkpoint += 1;
        match &entry {
            EntryTypes::Escrow(e) => own.escrows.push((hash.clone(), e.clone())),
            EntryTypes::Park(p) => own.parks.push((hash.clone(), p.clone())),
            EntryTypes::SettlementRun(r) => own.runs.push((hash.clone(), record.action().action_seq(), r.clone())),
            EntryTypes::Collect(c) => own.collects.push(c.clone()),
            EntryTypes::Reclaim(r) => {
                own.reclaims.push(r.clone());
                own.reclaims_by_park.insert(r.park.clone(), hash.clone());
            }
            EntryTypes::Checkpoint(_) => {
                own.latest_checkpoint = Some(hash.clone());
                own.checkpoint_seq = Some(record.action().action_seq());
                own.ledger_since_checkpoint = 0;
            }
            EntryTypes::Mint(_) => {}
        }
        own.ledger.push((hash, entry));
    }
    Ok(own)
}

/// The caller's ledger state from genesis, by the same arithmetic validation
/// uses (`dex_core::checkpoint`).
fn own_state(own: &OwnChain, props: &DexProperties) -> ExternResult<CheckpointState<ActionHash>> {
    let mut events = Vec::new();
    for (hash, entry) in &own.ledger {
        if let Some(event) = ledger_event(hash, entry, props)?.map_err(guest)? {
            events.push(event);
        }
    }
    CheckpointState::genesis().extend(&events).map_err(|e| guest(e.to_string()))
}

/// The checkpoint the next ledger write should cite, writing a new one first
/// when `CHECKPOINT_EVERY` ledger entries or `CHECKPOINT_AFTER_ACTIONS`
/// actions follow the last. A checkpoint equals the author's whole-chain
/// state, which is what validation re-derives from the previous one.
fn checkpoint_if_due() -> ExternResult<Option<ActionHash>> {
    let own = own_chain()?;
    let actions_since = match own.checkpoint_seq {
        Some(seq) => own.head_seq.saturating_sub(seq) as usize,
        None => own.head_seq as usize + 1,
    };
    if own.ledger_since_checkpoint < CHECKPOINT_EVERY && actions_since < CHECKPOINT_AFTER_ACTIONS {
        return Ok(own.latest_checkpoint);
    }
    let state = own_state(&own, &props()?)?;
    let hash = create_entry(&EntryTypes::Checkpoint(Checkpoint { prev: own.latest_checkpoint.clone(), state }))?;
    Ok(Some(hash))
}

/// The maker's latest action if it is stamped at or after `deadline`: an
/// anchor a Reclaim can cite.
fn maker_anchor(maker: &AgentPubKey, deadline: i64) -> ExternResult<Option<ActionHash>> {
    let status = get_agent_activity(
        maker.clone(),
        ChainQueryFilter::new(),
        ActivityRequest::Full,
        GetOptions::network(),
    )?;
    let Some((_, head)) = status.valid_activity.iter().max_by_key(|(seq, _)| *seq) else {
        return Ok(None);
    };
    let record = get_record(head)?;
    Ok((record.action().timestamp().as_micros() >= deadline).then(|| head.clone()))
}

fn collected_set(own: &OwnChain) -> BTreeSet<(ActionHash, u32)> {
    own.collects.iter().map(|c| (c.run.clone(), c.index)).collect()
}

/// Parks of this escrow that their takers have reclaimed.
fn reclaimed_parks(escrow: &ActionHash) -> ExternResult<BTreeSet<ActionHash>> {
    let mut parks = BTreeSet::new();
    for hash in link_targets(escrow.clone(), LinkTypes::EscrowToReclaims)? {
        if let Some(EntryTypes::Reclaim(r)) = decode_record(&get_record(&hash)?)? {
            parks.insert(r.park);
        }
    }
    Ok(parks)
}

fn link_targets(base: impl Into<AnyLinkableHash>, link_type: LinkTypes) -> ExternResult<Vec<ActionHash>> {
    let links = get_links(LinkQuery::try_new(base, link_type)?, GetStrategy::Network)?;
    let mut seen = BTreeSet::new();
    Ok(links
        .into_iter()
        .filter_map(|l| l.target.into_action_hash())
        .filter(|h| seen.insert(h.clone()))
        .collect())
}

/// A settlement run with the metadata readers need from its action.
struct EscrowRun {
    hash: ActionHash,
    seq: u32,
    /// The run's action timestamp: the `now` its validation used.
    at: Timestamp,
    run: SettlementRun,
}

/// An escrow's runs, oldest first (runs are all on the maker's chain, so
/// `action_seq` orders them).
fn escrow_runs(escrow: &ActionHash) -> ExternResult<Vec<EscrowRun>> {
    let mut runs = Vec::new();
    for hash in link_targets(escrow.clone(), LinkTypes::EscrowToRuns)? {
        let record = get_record(&hash)?;
        if let Some(EntryTypes::SettlementRun(run)) = decode_record(&record)? {
            runs.push(EscrowRun {
                hash,
                seq: record.action().action_seq(),
                at: record.action().timestamp(),
                run,
            });
        }
    }
    runs.sort_by_key(|r| r.seq);
    Ok(runs)
}

/// How many lots a consumed park was filled, by replaying the consuming run
/// through `execute_run` with the inputs its validation used.
fn replay_fill(
    escrow: &ActionHash,
    runs: &[EscrowRun],
    consuming: &EscrowRun,
    park: &ActionHash,
) -> ExternResult<u64> {
    let (escrow_record, entry) = get_escrow(escrow)?;
    let props = props()?;
    let market = market_of(&props, &entry)?;
    let prev_locked = match &consuming.run.prev_run {
        Some(prev) => {
            runs.iter()
                .find(|r| &r.hash == prev)
                .ok_or_else(|| guest(format!("previous run {prev} not found")))?
                .run
                .locked
                .clone()
        }
        None => entry.terms.initial_lock(market).map_err(core_err)?,
    };
    let mut parks = Vec::with_capacity(consuming.run.consumed.len());
    for hash in &consuming.run.consumed {
        let record = get_record(hash)?;
        let Some(EntryTypes::Park(p)) = decode_record(&record)? else {
            return Err(guest(format!("{hash} is not a park")));
        };
        parks.push(ParkInput {
            id: hash.clone(),
            taker: record.action().author().clone(),
            amounts: p.amounts,
            requested_lots: p.requested_lots,
            parked_at: record.action().timestamp().as_micros(),
        });
    }
    let output = execute_run(&RunInput {
        terms: entry.terms,
        market: market.clone(),
        maker: escrow_record.action().author().clone(),
        prev_locked,
        parks,
        now: consuming.at.as_micros(),
        mode: consuming.run.mode,
    })
    .map_err(core_err)?;
    output
        .outcomes
        .iter()
        .find(|o| &o.id == park)
        .map(|o| o.filled_lots)
        .ok_or_else(|| guest(format!("run {} did not consume {park}", consuming.hash)))
}

fn incoming_runs(agent: &AgentPubKey) -> ExternResult<Vec<(ActionHash, SettlementRun)>> {
    let mut runs = Vec::new();
    for hash in link_targets(agent.clone(), LinkTypes::AgentToIncomingRuns)? {
        let record = get_record(&hash)?;
        if let Some(EntryTypes::SettlementRun(run)) = decode_record(&record)? {
            runs.push((hash, run));
        }
    }
    Ok(runs)
}

fn pending_park_inputs(
    escrow: &ActionHash,
    consumed: &BTreeSet<ActionHash>,
) -> ExternResult<Vec<ParkInput<ActionHash, AgentPubKey>>> {
    let reclaimed = reclaimed_parks(escrow)?;
    let mut parks = Vec::new();
    for hash in link_targets(escrow.clone(), LinkTypes::EscrowToParks)? {
        if consumed.contains(&hash) || reclaimed.contains(&hash) {
            continue;
        }
        let record = get_record(&hash)?;
        if let Some(EntryTypes::Park(park)) = decode_record(&record)? {
            if &park.escrow != escrow {
                continue;
            }
            parks.push(ParkInput {
                id: hash,
                taker: record.action().author().clone(),
                amounts: park.amounts,
                requested_lots: park.requested_lots,
                parked_at: record.action().timestamp().as_micros(),
            });
        }
    }
    Ok(parks)
}
