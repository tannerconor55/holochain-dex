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
//!
//! Reads use `GetStrategy::Network` throughout. Whether the UI needs an
//! explicit local/network choice (zero-arc phones) is still an open decision.

use dex_core::{execute_run, select_parks, ParkInput, RunInput};
use hdk::prelude::*;
use ledger_api::{
    BalanceView, EscrowState, ParkFill, ParkRequest, ParkSettlement, ParkStatus, PendingPark,
    RunEscrowInput, RunReport,
};
use ledger_integrity::*;
use std::collections::BTreeSet;

// ---------------------------------------------------------------------------
// Writes
// ---------------------------------------------------------------------------

/// Test faucet. MVP only.
#[hdk_extern]
pub fn mint(amounts: Amounts) -> ExternResult<ActionHash> {
    create_entry(&EntryTypes::Mint(Mint { amounts }))
}

/// Open an order's escrow, locking the maker's funds. The returned hash is
/// the order's identity.
#[hdk_extern]
pub fn open_escrow(terms: OrderTerms) -> ExternResult<ActionHash> {
    let escrow = create_entry(&EntryTypes::Escrow(Escrow { terms }))?;
    create_link(my_key()?, escrow.clone(), LinkTypes::AgentToEscrows, ())?;
    Ok(escrow)
}

/// Park taker funds against a live order. Unused funds are refunded by the
/// maker's next run.
#[hdk_extern]
pub fn park(request: ParkRequest) -> ExternResult<ActionHash> {
    let park = create_entry(&EntryTypes::Park(Park {
        escrow: request.escrow.clone(),
        amounts: request.amounts,
        requested_lots: request.requested_lots,
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

    // Our own chain is authoritative for our runs: read it locally.
    let mine = own_ledger()?;
    let mut my_runs: Vec<&(ActionHash, u32, SettlementRun)> = mine
        .runs
        .iter()
        .filter(|(_, _, r)| r.escrow == input.escrow)
        .collect();
    my_runs.sort_by_key(|(_, seq, _)| std::cmp::Reverse(*seq));
    let prev_run = my_runs.first().map(|(hash, _, _)| hash.clone());
    let prev_locked = match my_runs.first() {
        Some((_, _, run)) => run.locked,
        None => escrow.terms.initial_lock().map_err(core_err)?,
    };
    let consumed: BTreeSet<ActionHash> = my_runs
        .iter()
        .flat_map(|(_, _, r)| r.consumed.iter().cloned())
        .collect();

    let pending = pending_park_inputs(&input.escrow, &consumed)?;
    let pending_count = pending.len();
    let parks = select_parks(pending);
    if input.mode == RunMode::Fill && parks.is_empty() {
        return Ok(None);
    }

    let run_input = RunInput {
        terms: escrow.terms,
        maker: me,
        prev_locked,
        parks,
        now: sys_time()?.as_micros(),
        mode: input.mode,
    };
    let output = execute_run(&run_input).map_err(core_err)?;

    let run = SettlementRun {
        escrow: input.escrow.clone(),
        prev_run,
        mode: input.mode,
        consumed: output.consumed.clone(),
        allocations: output.allocations.clone(),
        locked: output.locked,
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
    }))
}

/// Collect every allocation paid to the caller that isn't collected yet.
#[hdk_extern]
pub fn collect_all() -> ExternResult<Vec<ActionHash>> {
    let me = my_key()?;
    let collected = collected_set(&own_ledger()?);
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
                amounts: allocation.amounts,
            }))?);
        }
    }
    Ok(created)
}

// ---------------------------------------------------------------------------
// Reads
// ---------------------------------------------------------------------------

#[hdk_extern]
pub fn get_escrow_state(escrow: ActionHash) -> ExternResult<EscrowState> {
    let (record, entry) = get_escrow(&escrow)?;
    let maker = record.action().author().clone();
    let runs = escrow_runs(&escrow)?;

    // Walk the runs in order. Lots still unfilled when the order was released
    // were returned to the maker, not sold, so they don't count as filled.
    let mut locked = entry.terms.initial_lock().map_err(core_err)?;
    let mut unfilled_at_release = None;
    let mut released_at = None;
    for r in &runs {
        if r.run.mode == RunMode::Release && unfilled_at_release.is_none() {
            unfilled_at_release = Some(entry.terms.remaining_lots(&locked).map_err(core_err)?);
            released_at = Some(r.at);
        }
        locked = r.run.locked;
    }
    let remaining_lots = entry.terms.remaining_lots(&locked).map_err(core_err)?;
    let unfilled = unfilled_at_release.unwrap_or(remaining_lots);
    let released = unfilled_at_release.is_some();
    Ok(EscrowState {
        escrow,
        maker,
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
        out.push(ParkStatus {
            park: hash,
            escrow: park.escrow,
            amounts: park.amounts,
            requested_lots: park.requested_lots,
            parked_at: record.action().timestamp(),
            settlement,
        });
    }
    Ok(out)
}

/// Escrows the caller has opened, newest first.
#[hdk_extern]
pub fn get_my_escrows() -> ExternResult<Vec<ActionHash>> {
    let mut escrows = own_ledger()?.escrows;
    escrows.reverse();
    Ok(escrows.into_iter().map(|(hash, _)| hash).collect())
}

#[hdk_extern]
pub fn get_balance() -> ExternResult<BalanceView> {
    let me = my_key()?;
    let mine = own_ledger()?;
    let available = mine.available().map_err(core_err)?;

    let mut locked_in_escrows = Amounts::ZERO;
    for (escrow_hash, escrow) in &mine.escrows {
        let latest = mine
            .runs
            .iter()
            .filter(|(_, _, r)| &r.escrow == escrow_hash)
            .max_by_key(|(_, seq, _)| *seq);
        let lock = match latest {
            Some((_, _, run)) => run.locked,
            None => escrow.terms.initial_lock().map_err(core_err)?,
        };
        locked_in_escrows = add(locked_in_escrows, lock)?;
    }

    let mut parked = Amounts::ZERO;
    for (park_hash, park) in &mine.parks {
        let consumed = escrow_runs(&park.escrow)?
            .iter()
            .any(|r| r.run.consumed.contains(park_hash));
        if !consumed {
            parked = add(parked, park.amounts)?;
        }
    }

    let collected = collected_set(&mine);
    let mut uncollected = Amounts::ZERO;
    for (run_hash, run) in incoming_runs(&me)? {
        for (index, allocation) in run.allocations.iter().enumerate() {
            if allocation.receiver == me && !collected.contains(&(run_hash.clone(), index as u32)) {
                uncollected = add(uncollected, allocation.amounts)?;
            }
        }
    }

    let total = add(add(add(available, locked_in_escrows)?, parked)?, uncollected)?;
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

fn add(x: Amounts, y: Amounts) -> ExternResult<Amounts> {
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

/// The caller's ledger entries, read from their own source chain.
fn own_ledger() -> ExternResult<ChainLedger> {
    let mut ledger = ChainLedger::default();
    for record in query(ChainQueryFilter::new().include_entries(true))? {
        if let Some(entry) = decode_record(&record)? {
            ledger.push(
                record.action_address().clone(),
                record.action().action_seq(),
                entry,
            );
        }
    }
    Ok(ledger)
}

fn collected_set(ledger: &ChainLedger) -> BTreeSet<(ActionHash, u32)> {
    ledger
        .collects
        .iter()
        .map(|c| (c.run.clone(), c.index))
        .collect()
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
    let prev_locked = match &consuming.run.prev_run {
        Some(prev) => {
            runs.iter()
                .find(|r| &r.hash == prev)
                .ok_or_else(|| guest(format!("previous run {prev} not found")))?
                .run
                .locked
        }
        None => entry.terms.initial_lock().map_err(core_err)?,
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
    let mut parks = Vec::new();
    for hash in link_targets(escrow.clone(), LinkTypes::EscrowToParks)? {
        if consumed.contains(&hash) {
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
