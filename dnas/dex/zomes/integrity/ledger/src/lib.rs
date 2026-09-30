//! # ledger_integrity — mock Unyt ledger
//!
//! A deliberately small stand-in for Unyt's accounting and Smart Agreement
//! layer, built so the DEX can be developed before Unyt API access exists.
//! It mirrors the design planned for the real Unyt template:
//!
//! | Mock entry       | Unyt analogue                                        |
//! |------------------|------------------------------------------------------|
//! | `Mint`           | test-network issuance (faucet; MVP only)             |
//! | `Escrow`         | a Smart Agreement instance + the maker's locked spend|
//! | `Park`           | a taker's `ParkedSpendBalance` + request data        |
//! | `SettlementRun`  | a RAVE executed by the maker (`AuthorizedExecutor`)  |
//! | `Collect`        | a receiver collecting an allocation                  |
//! | `Reclaim`        | a taker withdrawing a park the maker never settled   |
//! | `Checkpoint`     | an author's running ledger totals                    |
//!
//! Units, markets and the park timeout come from the DNA properties
//! (`dex_core::properties`); a DNA whose properties fail the rules cannot be
//! joined (`genesis_self_check`) and validation never falls back to a default.
//!
//! ## What validation guarantees
//!
//! * **Balances never go negative.** Every debit (`Escrow`, `Park`) is checked
//!   against the author's balance: their latest `Checkpoint` plus the ledger
//!   entries after it, never the whole chain.
//! * **Double-spend is impossible on an unforked chain.** Only the escrow's
//!   maker can write its settlement runs, each run must name the maker's
//!   latest run for that escrow as `prev_run`, and a park can be consumed by
//!   at most one run.
//! * **Run or reclaim, never both** (design doc section 4.2). A run may
//!   consume a park only before the park's deadline; a `Reclaim` must cite a
//!   maker action at or after the deadline, and the maker's chain from the
//!   escrow to that action must hold no run consuming the park. A fork gets
//!   the maker warranted by Holochain; it is not prevented here.
//! * **Atomic settlement.** A run is re-executed by every validator with
//!   [`dex_core::execute_run`]; its allocations and lock must match exactly,
//!   and the result conserves every unit.
//! * **No double collection**, across checkpoints: a checkpoint carries every
//!   allocation its author ever collected.
//!
//! ## Known MVP simplifications
//!
//! * `Mint` is an unrestricted faucet (capped per mint). Test assets only.
//! * Walks trust that the author's earlier entries were valid when committed;
//!   invalid ones are caught by their own validation.
//! * The run timestamp is the maker's own action timestamp, as Unyt's
//!   `executed_timestamp` is the executor's.

use dex_core::checkpoint::{CheckpointState, LedgerEvent, MAX_ACTIONS_SINCE_CHECKPOINT};
use dex_core::properties::DexProperties;
use dex_core::timeout::{park_deadline, reclaim_valid, run_may_consume, MakerAction};
use dex_core::{execute_run, CoreError, MarketId, ParkInput, RunInput, MAX_MINT, MAX_PARKS_PER_RUN};
use hdi::prelude::*;
use std::collections::BTreeSet;

pub use dex_core::{Allocation, Amounts, MarketDef, OrderTerms, RunMode, Side};

// ---------------------------------------------------------------------------
// Entry types
// ---------------------------------------------------------------------------

/// Test-faucet issuance to the author.
#[hdk_entry_helper]
#[derive(Clone, PartialEq)]
pub struct Mint {
    pub amounts: Amounts,
}

/// Defined in `ledger_api` so the dex integrity zome can decode it too.
pub use ledger_api::Escrow;

/// A taker's parked funds and request against one escrow.
#[hdk_entry_helper]
#[derive(Clone, PartialEq)]
pub struct Park {
    pub escrow: ActionHash,
    /// Must equal the escrow's market.
    pub market: MarketId,
    pub amounts: Amounts,
    pub requested_lots: u64,
    /// The author's latest checkpoint when parking.
    pub checkpoint: Option<ActionHash>,
}

/// One execution of an escrow's settlement logic, authored by its maker.
#[hdk_entry_helper]
#[derive(Clone, PartialEq)]
pub struct SettlementRun {
    pub escrow: ActionHash,
    /// The maker's previous run for this escrow; `None` for the first run.
    pub prev_run: Option<ActionHash>,
    pub mode: RunMode,
    /// Parks consumed, in processing (time-priority) order.
    pub consumed: Vec<ActionHash>,
    pub allocations: Vec<Allocation<AgentPubKey>>,
    pub locked: Amounts,
}

/// A receiver collecting one allocation of a settlement run into their balance.
#[hdk_entry_helper]
#[derive(Clone, PartialEq)]
pub struct Collect {
    pub run: ActionHash,
    pub index: u32,
    pub amounts: Amounts,
    /// The author's latest checkpoint when collecting.
    pub checkpoint: Option<ActionHash>,
}

/// A taker taking back a park the maker never settled.
#[hdk_entry_helper]
#[derive(Clone, PartialEq)]
pub struct Reclaim {
    pub park: ActionHash,
    /// Any action on the maker's chain stamped at or after the park's deadline.
    pub anchor: ActionHash,
}

/// An author's cumulative ledger totals and collected allocations.
#[hdk_entry_helper]
#[derive(Clone, PartialEq)]
pub struct Checkpoint {
    /// The author's previous checkpoint; `None` for the first.
    pub prev: Option<ActionHash>,
    pub state: CheckpointState<ActionHash>,
}

#[hdk_entry_types]
#[unit_enum(UnitEntryTypes)]
pub enum EntryTypes {
    #[entry_type(cache_at_agent_activity = true)]
    Mint(Mint),
    #[entry_type(cache_at_agent_activity = true)]
    Escrow(Escrow),
    #[entry_type(cache_at_agent_activity = true)]
    Park(Park),
    #[entry_type(cache_at_agent_activity = true)]
    SettlementRun(SettlementRun),
    #[entry_type(cache_at_agent_activity = true)]
    Collect(Collect),
    #[entry_type(cache_at_agent_activity = true)]
    Reclaim(Reclaim),
    #[entry_type(cache_at_agent_activity = true)]
    Checkpoint(Checkpoint),
}

// Other integrity zomes identify escrows by this index (see `ledger_api`).
const _: () = assert!(UnitEntryTypes::Escrow as u8 == ledger_api::ESCROW_ENTRY_INDEX);

#[hdk_link_types]
pub enum LinkTypes {
    /// Escrow → parks against it. Lets the maker find pending takers.
    EscrowToParks,
    /// Escrow → its settlement runs. Lets anyone read the order's state.
    EscrowToRuns,
    /// Receiver agent → runs that allocated to them. Lets them collect.
    AgentToIncomingRuns,
    /// Maker agent → their escrows.
    AgentToEscrows,
    /// Escrow → reclaims of its parks. Lets the maker skip reclaimed parks.
    EscrowToReclaims,
}

// ---------------------------------------------------------------------------
// DNA properties
// ---------------------------------------------------------------------------

/// The DNA properties, checked. `Err` holds why they are unusable; callers
/// refuse rather than fall back to a default.
pub fn load_properties() -> ExternResult<Result<DexProperties, String>> {
    let bytes = dna_info()?.modifiers.properties;
    let props: DexProperties = match holochain_serialized_bytes::decode(bytes.bytes()) {
        Ok(props) => props,
        Err(e) => return Ok(Err(format!("malformed DNA properties: {e}"))),
    };
    Ok(props.check().map(|()| props).map_err(|e| format!("invalid DNA properties: {e}")))
}

/// An agent cannot join a DNA whose properties are missing or malformed.
#[hdk_extern]
pub fn genesis_self_check(_data: GenesisSelfCheckData) -> ExternResult<ValidateCallbackResult> {
    match load_properties()? {
        Ok(_) => valid(),
        Err(why) => invalid(why),
    }
}

// ---------------------------------------------------------------------------
// Validation
// ---------------------------------------------------------------------------

fn valid() -> ExternResult<ValidateCallbackResult> {
    Ok(ValidateCallbackResult::Valid)
}

fn invalid(reason: impl Into<String>) -> ExternResult<ValidateCallbackResult> {
    Ok(ValidateCallbackResult::Invalid(reason.into()))
}

macro_rules! ensure {
    ($cond:expr, $($msg:tt)+) => {
        if !$cond {
            return invalid(format!($($msg)+));
        }
    };
}

/// Unwrap a `Result<T, String>` or return it as the Invalid verdict.
macro_rules! valid_or {
    ($e:expr) => {
        match $e {
            Ok(v) => v,
            Err(why) => return invalid(why),
        }
    };
}

#[hdk_extern]
pub fn validate(op: Op) -> ExternResult<ValidateCallbackResult> {
    match op.flattened::<EntryTypes, LinkTypes>()? {
        FlatOp::CreateRecord(OpRecord::CreateEntry { app_entry, action })
        | FlatOp::CreateEntry(OpEntry::CreateEntry { app_entry, action }) => {
            let props = valid_or!(load_properties()?);
            validate_create(app_entry, &action, &props)
        }
        FlatOp::CreateRecord(OpRecord::UpdateEntry { .. })
        | FlatOp::CreateEntry(OpEntry::UpdateEntry { .. })
        | FlatOp::CreateRecord(OpRecord::DeleteEntry { .. }) => {
            invalid("ledger entries are immutable")
        }
        FlatOp::CreateRecord(OpRecord::CreateLink { link_type, action })
        | FlatOp::Link(OpLink::CreateLink { link_type, action }) => {
            validate_create_link(link_type, &action)
        }
        FlatOp::CreateRecord(OpRecord::DeleteLink { .. }) | FlatOp::Link(OpLink::DeleteLink { .. }) => {
            invalid("ledger links are permanent")
        }
        _ => valid(),
    }
}

type CreateAction = TypedAction<CreateData>;

/// The action before this one. Every app entry follows genesis, so a missing
/// `prev_action` is a malformed op, not an invalid one.
fn prev_action<D>(action: &TypedAction<D>) -> ExternResult<ActionHash> {
    action
        .prev_action()
        .cloned()
        .ok_or_else(|| wasm_error!(WasmErrorInner::Guest("app action has no prev_action".into())))
}

fn validate_create(entry: EntryTypes, action: &CreateAction, props: &DexProperties) -> ExternResult<ValidateCallbackResult> {
    match entry {
        EntryTypes::Mint(mint) => validate_mint(&mint, props),
        EntryTypes::Escrow(escrow) => validate_escrow(&escrow, action, props),
        EntryTypes::Park(park) => validate_park(&park, action, props),
        EntryTypes::SettlementRun(run) => validate_run(&run, action, props),
        EntryTypes::Collect(collect) => validate_collect(&collect, action, props),
        EntryTypes::Reclaim(reclaim) => validate_reclaim(&reclaim, action, props),
        EntryTypes::Checkpoint(checkpoint) => validate_checkpoint(&checkpoint, action, props),
    }
}

fn validate_mint(mint: &Mint, props: &DexProperties) -> ExternResult<ValidateCallbackResult> {
    ensure!(!mint.amounts.is_zero(), "mint must create something");
    valid_or!(props.check_amounts(&mint.amounts).map_err(|e| e.to_string()));
    ensure!(
        mint.amounts.units().all(|(_, amount)| amount <= MAX_MINT),
        "mint exceeds the per-mint cap of {MAX_MINT}"
    );
    valid()
}

fn validate_escrow(escrow: &Escrow, action: &CreateAction, props: &DexProperties) -> ExternResult<ValidateCallbackResult> {
    let market = valid_or!(props.market(&escrow.market).map_err(|e| e.to_string()));
    if let Err(e) = escrow.terms.validate(market) {
        return invalid(e.to_string());
    }
    ensure!(
        escrow.terms.expires_at > action.timestamp().as_micros(),
        "escrow must expire after it is created"
    );
    let lock = escrow.terms.initial_lock(market).map_err(core_err)?;
    validate_debit(action, &escrow.checkpoint, &lock, props)
}

fn validate_park(park: &Park, action: &CreateAction, props: &DexProperties) -> ExternResult<ValidateCallbackResult> {
    ensure!(park.requested_lots > 0, "a park must request at least one lot");
    ensure!(!park.amounts.is_zero(), "a park must commit funds");
    valid_or!(props.check_amounts(&park.amounts).map_err(|e| e.to_string()));
    let escrow_record = must_get_valid_record(park.escrow.clone())?;
    let Some(EntryTypes::Escrow(escrow)) = decode_record(&escrow_record)? else {
        return invalid("park must reference an escrow");
    };
    ensure!(park.market == escrow.market, "a park must be in its escrow's market");
    validate_debit(action, &park.checkpoint, &park.amounts, props)
}

fn validate_run(run: &SettlementRun, action: &CreateAction, props: &DexProperties) -> ExternResult<ValidateCallbackResult> {
    ensure!(
        run.consumed.len() <= MAX_PARKS_PER_RUN,
        "a run may consume at most {MAX_PARKS_PER_RUN} parks"
    );

    // Only the maker may run their escrow: the AuthorizedExecutor rule.
    let escrow_record = must_get_valid_record(run.escrow.clone())?;
    let Some(EntryTypes::Escrow(escrow)) = decode_record(&escrow_record)? else {
        return invalid("run must reference an escrow");
    };
    ensure!(
        escrow_record.action().author() == action.author(),
        "only the escrow's maker may execute its settlement runs"
    );
    let market = valid_or!(props.market(&escrow.market).map_err(|e| e.to_string()));
    let timing = valid_or!(props.timing().map_err(|e| e.to_string()));

    // `prev_run` must be the maker's latest run for this escrow, so the lock
    // can't be spent twice: no run for this escrow may sit between it (or the
    // escrow, for the first run) and this run.
    let bottom = run.prev_run.clone().unwrap_or_else(|| run.escrow.clone());
    for item in chain_segment(action.author(), &prev_action(action)?, Some(&bottom))? {
        if let Some(EntryTypes::SettlementRun(between)) = &item.entry {
            ensure!(
                between.escrow != run.escrow || item.hash == bottom,
                "prev_run must be the maker's latest run for this escrow"
            );
        }
    }

    // The lock carried from the previous run, and every park consumed so far,
    // by following the prev_run chain: bounded by this order's life.
    let mut prev_locked = escrow.terms.initial_lock(market).map_err(core_err)?;
    let mut already_consumed: BTreeSet<ActionHash> = BTreeSet::new();
    let mut next = run.prev_run.clone();
    let mut first = true;
    while let Some(hash) = next {
        let record = must_get_valid_record(hash.clone())?;
        let Some(EntryTypes::SettlementRun(earlier)) = decode_record(&record)? else {
            return invalid("prev_run must be a settlement run");
        };
        ensure!(earlier.escrow == run.escrow, "prev_run must be a run of the same escrow");
        if first {
            prev_locked = earlier.locked.clone();
            first = false;
        }
        already_consumed.extend(earlier.consumed.iter().cloned());
        next = earlier.prev_run;
    }

    // Resolve every consumed park: this escrow's, once ever, before its deadline.
    let now = action.timestamp().as_micros();
    let mut parks = Vec::with_capacity(run.consumed.len());
    for park_hash in &run.consumed {
        ensure!(
            !already_consumed.contains(park_hash),
            "park {park_hash} was already consumed by an earlier run"
        );
        let park_record = must_get_valid_record(park_hash.clone())?;
        let Some(EntryTypes::Park(park)) = decode_record(&park_record)? else {
            return invalid(format!("{park_hash} is not a park"));
        };
        ensure!(park.escrow == run.escrow, "park {park_hash} targets a different escrow");
        let parked_at = park_record.action().timestamp().as_micros();
        let Some(deadline) = park_deadline(parked_at, escrow.terms.expires_at, &timing) else {
            return invalid("park deadline overflows");
        };
        ensure!(
            run_may_consume(now, deadline),
            "park {park_hash} is past its deadline and may only be reclaimed"
        );
        parks.push(ParkInput {
            id: park_hash.clone(),
            taker: park_record.action().author().clone(),
            amounts: park.amounts,
            requested_lots: park.requested_lots,
            parked_at,
        });
    }

    // Re-execute the settlement logic and require an exact match.
    let input = RunInput {
        terms: escrow.terms,
        market: market.clone(),
        maker: action.author().clone(),
        prev_locked,
        parks,
        now,
        mode: run.mode,
    };
    let output = match execute_run(&input) {
        Ok(output) => output,
        Err(e) => return invalid(format!("settlement logic rejected the run: {e}")),
    };
    ensure!(output.consumed == run.consumed, "consumed parks are not in time-priority order");
    ensure!(output.allocations == run.allocations, "allocations do not match the settlement logic");
    ensure!(output.locked == run.locked, "locked amount does not match the settlement logic");
    valid()
}

fn validate_collect(collect: &Collect, action: &CreateAction, props: &DexProperties) -> ExternResult<ValidateCallbackResult> {
    let run_record = must_get_valid_record(collect.run.clone())?;
    let Some(EntryTypes::SettlementRun(run)) = decode_record(&run_record)? else {
        return invalid("collect must reference a settlement run");
    };
    let Some(allocation) = run.allocations.get(collect.index as usize) else {
        return invalid("allocation index out of range");
    };
    ensure!(
        &allocation.receiver == action.author(),
        "only the receiver may collect an allocation"
    );
    ensure!(
        allocation.amounts == collect.amounts,
        "collected amounts must equal the allocation"
    );
    let state = valid_or!(ledger_state(action, &collect.checkpoint, true, props)?);
    ensure!(
        !state.is_collected(&collect.run, collect.index),
        "allocation already collected"
    );
    valid()
}

fn validate_reclaim(reclaim: &Reclaim, action: &CreateAction, props: &DexProperties) -> ExternResult<ValidateCallbackResult> {
    let park_record = must_get_valid_record(reclaim.park.clone())?;
    let Some(EntryTypes::Park(park)) = decode_record(&park_record)? else {
        return invalid("a reclaim must reference a park");
    };
    ensure!(
        park_record.action().author() == action.author(),
        "only the park's author may reclaim it"
    );
    let escrow_record = must_get_valid_record(park.escrow.clone())?;
    let Some(EntryTypes::Escrow(escrow)) = decode_record(&escrow_record)? else {
        return invalid("the park's escrow is not an escrow");
    };
    let maker = escrow_record.action().author().clone();
    let anchor_record = must_get_valid_record(reclaim.anchor.clone())?;
    ensure!(
        anchor_record.action().author() == &maker,
        "the anchor must be an action on the maker's chain"
    );
    let timing = valid_or!(props.timing().map_err(|e| e.to_string()));
    let Some(deadline) = park_deadline(
        park_record.action().timestamp().as_micros(),
        escrow.terms.expires_at,
        &timing,
    ) else {
        return invalid("park deadline overflows");
    };

    // The maker's chain from the escrow up to the anchor, oldest first. It
    // must reach the escrow: a run before a shorter walk's start would be
    // missed. The one rule both validation and the property test use.
    let walk: Vec<MakerAction> = chain_segment(&maker, &reclaim.anchor, Some(&park.escrow))?
        .into_iter()
        .map(|item| MakerAction {
            timestamp: item.timestamp,
            consumes_park: matches!(&item.entry, Some(EntryTypes::SettlementRun(r)) if r.consumed.contains(&reclaim.park)),
        })
        .collect();
    if let Err(refusal) = reclaim_valid(&walk, deadline) {
        return invalid(refusal.to_string());
    }

    // One reclaim per park: none between the park and this action.
    for item in chain_segment(action.author(), &prev_action(action)?, Some(&reclaim.park))? {
        if let Some(EntryTypes::Reclaim(earlier)) = &item.entry {
            ensure!(earlier.park != reclaim.park, "this park was already reclaimed");
        }
    }
    valid()
}

fn validate_checkpoint(checkpoint: &Checkpoint, action: &CreateAction, props: &DexProperties) -> ExternResult<ValidateCallbackResult> {
    // Exempt from the segment limit, so an author past it can always recover.
    let expected = valid_or!(ledger_state(action, &checkpoint.prev, false, props)?);
    ensure!(
        expected == checkpoint.state,
        "checkpoint totals do not equal the previous checkpoint plus the entries since"
    );
    valid()
}

fn validate_debit(
    action: &CreateAction,
    checkpoint: &Option<ActionHash>,
    debit: &Amounts,
    props: &DexProperties,
) -> ExternResult<ValidateCallbackResult> {
    let state = valid_or!(ledger_state(action, checkpoint, true, props)?);
    let available = valid_or!(state.totals.available().map_err(|e| e.to_string()));
    ensure!(
        available.covers(debit),
        "insufficient balance: available {available:?}, required {debit:?}"
    );
    valid()
}

fn validate_create_link(link_type: LinkTypes, action: &TypedAction<CreateLinkData>) -> ExternResult<ValidateCallbackResult> {
    let base = action.data.base_address.clone();
    let target = action.data.target_address.clone();
    // Every ledger link's target is a ledger entry authored by the link author.
    let Some(target_hash) = target.into_action_hash() else {
        return invalid("link target must be an action hash");
    };
    let target_record = must_get_valid_record(target_hash)?;
    ensure!(
        target_record.action().author() == action.author(),
        "ledger links must be created by the target's author"
    );
    let target_entry = decode_record(&target_record)?;
    match (link_type, target_entry) {
        (LinkTypes::EscrowToParks, Some(EntryTypes::Park(park))) => {
            ensure!(base.into_action_hash() == Some(park.escrow), "base must be the park's escrow");
        }
        (LinkTypes::EscrowToRuns, Some(EntryTypes::SettlementRun(run))) => {
            ensure!(base.into_action_hash() == Some(run.escrow), "base must be the run's escrow");
        }
        (LinkTypes::AgentToIncomingRuns, Some(EntryTypes::SettlementRun(run))) => {
            let Some(agent) = base.into_agent_pub_key() else {
                return invalid("base must be an agent");
            };
            ensure!(
                run.allocations.iter().any(|a| a.receiver == agent),
                "run allocates nothing to this agent"
            );
        }
        (LinkTypes::AgentToEscrows, Some(EntryTypes::Escrow(_))) => {
            ensure!(
                base.into_agent_pub_key() == Some(action.author().clone()),
                "base must be the maker"
            );
        }
        (LinkTypes::EscrowToReclaims, Some(EntryTypes::Reclaim(reclaim))) => {
            let park_record = must_get_valid_record(reclaim.park)?;
            let Some(EntryTypes::Park(park)) = decode_record(&park_record)? else {
                return invalid("the reclaimed park is not a park");
            };
            ensure!(base.into_action_hash() == Some(park.escrow), "base must be the park's escrow");
        }
        _ => return invalid("link target has the wrong entry type"),
    }
    valid()
}

fn core_err(e: CoreError) -> WasmError {
    wasm_error!(WasmErrorInner::Guest(e.to_string()))
}

// ---------------------------------------------------------------------------
// Chain reading (shared with the coordinator)
// ---------------------------------------------------------------------------

/// Decode a record's entry using the entry type recorded in its action, never
/// by trial deserialization (a `Collect` would otherwise decode as a `Mint`).
pub fn decode_record(record: &Record) -> ExternResult<Option<EntryTypes>> {
    let Some(entry) = record.entry().as_option() else {
        return Ok(None);
    };
    decode_entry(record.action(), entry)
}

fn decode_entry(action: &Action, entry: &Entry) -> ExternResult<Option<EntryTypes>> {
    match action.entry_data() {
        Some((_, EntryType::App(def))) => {
            EntryTypes::deserialize_from_type(def.zome_index, def.entry_index, entry)
        }
        _ => Ok(None),
    }
}

/// One action of a walked chain segment.
pub struct ChainItem {
    pub hash: ActionHash,
    pub seq: u32,
    pub timestamp: i64,
    pub entry: Option<EntryTypes>,
}

/// `author`'s chain from `top` down to and including `bottom` (or to genesis
/// when `None`), oldest first, with ledger entries decoded. Deterministic:
/// `must_get_agent_activity` with a hash-bounded filter follows
/// `prev_action` from `top`.
pub fn chain_segment(author: &AgentPubKey, top: &ActionHash, bottom: Option<&ActionHash>) -> ExternResult<Vec<ChainItem>> {
    let filter = match bottom {
        Some(bottom) => ChainFilter::until_hash(top.clone(), bottom.clone()),
        None => ChainFilter::new(top.clone()),
    }
    .include_cached_entries();
    let mut items = Vec::new();
    for activity in must_get_agent_activity(author.clone(), filter)? {
        let action = &activity.action.hashed.content;
        let entry = match action.entry_data() {
            Some((entry_hash, EntryType::App(_))) => {
                let entry = match activity.cached_entry {
                    Some(entry) => entry,
                    None => must_get_entry(entry_hash.clone())?.content,
                };
                decode_entry(action, &entry)?
            }
            _ => None,
        };
        items.push(ChainItem {
            hash: activity.action.hashed.hash.clone(),
            seq: action.action_seq(),
            timestamp: action.timestamp().as_micros(),
            entry,
        });
    }
    items.sort_by_key(|i| i.seq);
    Ok(items)
}

/// The checkpoint arithmetic's view of one ledger entry. A reclaim credits
/// back its park's amounts.
pub fn ledger_event(hash: &ActionHash, entry: &EntryTypes, props: &DexProperties) -> ExternResult<Result<Option<LedgerEvent<ActionHash>>, String>> {
    let _ = hash;
    Ok(Ok(match entry {
        EntryTypes::Mint(m) => Some(LedgerEvent::Mint(m.amounts.clone())),
        EntryTypes::Escrow(e) => {
            let market = match props.market(&e.market) {
                Ok(market) => market,
                Err(why) => return Ok(Err(why.to_string())),
            };
            Some(LedgerEvent::Escrow(e.terms.initial_lock(market).map_err(core_err)?))
        }
        EntryTypes::Park(p) => Some(LedgerEvent::Park(p.amounts.clone())),
        EntryTypes::Collect(c) => Some(LedgerEvent::Collect {
            run: c.run.clone(),
            index: c.index,
            amounts: c.amounts.clone(),
        }),
        EntryTypes::Reclaim(r) => {
            let record = must_get_valid_record(r.park.clone())?;
            match decode_record(&record)? {
                Some(EntryTypes::Park(p)) => Some(LedgerEvent::Reclaim(p.amounts)),
                _ => return Ok(Err("a reclaim's park is not a park".into())),
            }
        }
        EntryTypes::SettlementRun(_) | EntryTypes::Checkpoint(_) => None,
    }))
}

/// The author's ledger state just before `action`: the cited checkpoint (or
/// genesis) extended by every ledger entry after it. Refuses a stale citation
/// (a newer checkpoint sits in between), a checkpoint not by the author, and,
/// when `limited`, a segment longer than `MAX_ACTIONS_SINCE_CHECKPOINT`.
fn ledger_state(
    action: &CreateAction,
    checkpoint: &Option<ActionHash>,
    limited: bool,
    props: &DexProperties,
) -> ExternResult<Result<CheckpointState<ActionHash>, String>> {
    let base = match checkpoint {
        Some(hash) => {
            let record = must_get_valid_record(hash.clone())?;
            if record.action().author() != action.author() {
                return Ok(Err("the cited checkpoint is not the author's".into()));
            }
            match decode_record(&record)? {
                Some(EntryTypes::Checkpoint(c)) => c.state,
                _ => return Ok(Err("the cited checkpoint is not a checkpoint".into())),
            }
        }
        None => CheckpointState::genesis(),
    };
    let segment = chain_segment(action.author(), &prev_action(action)?, checkpoint.as_ref())?;
    let after: Vec<&ChainItem> = segment.iter().filter(|i| Some(&i.hash) != checkpoint.as_ref()).collect();
    if limited && after.len() > MAX_ACTIONS_SINCE_CHECKPOINT {
        return Ok(Err(format!(
            "{} actions since the cited checkpoint, more than {MAX_ACTIONS_SINCE_CHECKPOINT}: write a checkpoint first",
            after.len()
        )));
    }
    let mut events = Vec::new();
    for item in after {
        let Some(entry) = &item.entry else { continue };
        if matches!(entry, EntryTypes::Checkpoint(_)) {
            return Ok(Err("a newer checkpoint exists: cite the latest".into()));
        }
        match ledger_event(&item.hash, entry, props)? {
            Ok(Some(event)) => events.push(event),
            Ok(None) => {}
            Err(why) => return Ok(Err(why)),
        }
    }
    Ok(base.extend(&events).map_err(|e| e.to_string()))
}
