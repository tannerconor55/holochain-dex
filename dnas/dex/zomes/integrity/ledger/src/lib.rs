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
//!
//! ## What validation guarantees
//!
//! * **Balances never go negative.** Every debit (`Escrow`, `Park`) is checked
//!   against the author's balance, recomputed from their own source chain.
//! * **Double-spend is impossible.** Only the escrow's maker can write its
//!   settlement runs, each run must name the maker's latest run for that escrow
//!   as `prev_run`, and a park can be consumed by at most one run.
//! * **Atomic settlement.** A run is re-executed by every validator with
//!   [`dex_core::execute_run`]; its allocations and lock must match exactly,
//!   and the result conserves both assets.
//! * **No double collection.** An allocation can be collected once, only by
//!   its receiver.
//!
//! ## Known MVP simplifications
//!
//! * `Mint` is an unrestricted faucet (capped per mint). Test assets only.
//! * Balance checks walk the author's whole chain: O(chain length). Fine for a
//!   demo; the real ledger is Unyt's.
//! * Chain walks trust that the author's earlier entries were themselves valid
//!   when committed. A node that commits invalid entries is caught by the
//!   validation of those entries, not re-checked here.
//! * The run timestamp is the maker's own action timestamp, as Unyt's
//!   `executed_timestamp` is the executor's. Expiry protects the maker.

use dex_core::{execute_run, CoreError, ParkInput, RunInput, MAX_MINT, MAX_PARKS_PER_RUN};
use hdi::prelude::*;
use std::collections::BTreeSet;

pub use dex_core::{Allocation, Amounts, Asset, OrderTerms, RunMode, Side, LOT_SIZE_A};

// ---------------------------------------------------------------------------
// Entry types
// ---------------------------------------------------------------------------

/// Test-faucet issuance to the author.
#[hdk_entry_helper]
#[derive(Clone, PartialEq)]
pub struct Mint {
    pub amounts: Amounts,
}

/// An order's escrow. Creating it locks `terms.initial_lock()` from the maker.
/// The action hash of this entry is the order's identity.
#[hdk_entry_helper]
#[derive(Clone, PartialEq)]
pub struct Escrow {
    pub terms: OrderTerms,
}

/// A taker's parked funds and request against one escrow.
#[hdk_entry_helper]
#[derive(Clone, PartialEq)]
pub struct Park {
    pub escrow: ActionHash,
    pub amounts: Amounts,
    pub requested_lots: u64,
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
}

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

#[hdk_extern]
pub fn validate(op: Op) -> ExternResult<ValidateCallbackResult> {
    match op.flattened::<EntryTypes, LinkTypes>()? {
        FlatOp::StoreRecord(OpRecord::CreateEntry { app_entry, action }) => {
            validate_create(app_entry, &action)
        }
        FlatOp::StoreEntry(OpEntry::CreateEntry { app_entry, action }) => {
            validate_create(app_entry, &action)
        }
        FlatOp::StoreRecord(OpRecord::UpdateEntry { .. })
        | FlatOp::StoreEntry(OpEntry::UpdateEntry { .. })
        | FlatOp::StoreRecord(OpRecord::DeleteEntry { .. }) => {
            invalid("ledger entries are immutable")
        }
        FlatOp::StoreRecord(OpRecord::CreateLink {
            base_address,
            target_address,
            link_type,
            action,
            ..
        })
        | FlatOp::RegisterCreateLink {
            base_address,
            target_address,
            link_type,
            action,
            ..
        } => validate_create_link(link_type, base_address, target_address, &action),
        FlatOp::StoreRecord(OpRecord::DeleteLink { .. }) | FlatOp::RegisterDeleteLink { .. } => {
            invalid("ledger links are permanent")
        }
        _ => valid(),
    }
}

fn validate_create(entry: EntryTypes, action: &Create) -> ExternResult<ValidateCallbackResult> {
    match entry {
        EntryTypes::Mint(mint) => validate_mint(&mint),
        EntryTypes::Escrow(escrow) => validate_escrow(&escrow, action),
        EntryTypes::Park(park) => validate_park(&park, action),
        EntryTypes::SettlementRun(run) => validate_run(&run, action),
        EntryTypes::Collect(collect) => validate_collect(&collect, action),
    }
}

fn validate_mint(mint: &Mint) -> ExternResult<ValidateCallbackResult> {
    ensure!(!mint.amounts.is_zero(), "mint must create something");
    ensure!(
        mint.amounts.a <= MAX_MINT && mint.amounts.b <= MAX_MINT,
        "mint exceeds the per-mint cap of {MAX_MINT}"
    );
    valid()
}

fn validate_escrow(escrow: &Escrow, action: &Create) -> ExternResult<ValidateCallbackResult> {
    if let Err(e) = escrow.terms.validate() {
        return invalid(e.to_string());
    }
    ensure!(
        escrow.terms.expires_at > action.timestamp.as_micros(),
        "escrow must expire after it is created"
    );
    let lock = escrow.terms.initial_lock().map_err(core_err)?;
    validate_debit(action, lock)
}

fn validate_park(park: &Park, action: &Create) -> ExternResult<ValidateCallbackResult> {
    ensure!(park.requested_lots > 0, "a park must request at least one lot");
    ensure!(!park.amounts.is_zero(), "a park must commit funds");
    let escrow_record = must_get_valid_record(park.escrow.clone())?;
    ensure!(
        matches!(decode_record(&escrow_record)?, Some(EntryTypes::Escrow(_))),
        "park must reference an escrow"
    );
    validate_debit(action, park.amounts)
}

fn validate_run(run: &SettlementRun, action: &Create) -> ExternResult<ValidateCallbackResult> {
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
        escrow_record.action().author() == &action.author,
        "only the escrow's maker may execute its settlement runs"
    );

    // The run chain for this escrow must be linear on the maker's source chain:
    // `prev_run` must be the latest earlier run, so the lock can't be spent twice.
    let history = walk_chain(&action.author, &action.prev_action)?;
    let mut earlier_runs: Vec<&(ActionHash, u32, SettlementRun)> = history
        .runs
        .iter()
        .filter(|(_, _, r)| r.escrow == run.escrow)
        .collect();
    earlier_runs.sort_by_key(|(_, seq, _)| std::cmp::Reverse(*seq));
    let latest = earlier_runs.first();
    ensure!(
        run.prev_run.as_ref() == latest.map(|(hash, _, _)| hash),
        "prev_run must be the maker's latest run for this escrow"
    );
    let prev_locked = match latest {
        Some((_, _, prev)) => prev.locked,
        None => escrow.terms.initial_lock().map_err(core_err)?,
    };
    let already_consumed: BTreeSet<&ActionHash> = earlier_runs
        .iter()
        .flat_map(|(_, _, r)| r.consumed.iter())
        .collect();

    // Resolve every consumed park; each must target this escrow, once ever.
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
        parks.push(ParkInput {
            id: park_hash.clone(),
            taker: park_record.action().author().clone(),
            amounts: park.amounts,
            requested_lots: park.requested_lots,
            parked_at: park_record.action().timestamp().as_micros(),
        });
    }

    // Re-execute the settlement logic and require an exact match.
    let input = RunInput {
        terms: escrow.terms,
        maker: action.author.clone(),
        prev_locked,
        parks,
        now: action.timestamp.as_micros(),
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

fn validate_collect(collect: &Collect, action: &Create) -> ExternResult<ValidateCallbackResult> {
    let run_record = must_get_valid_record(collect.run.clone())?;
    let Some(EntryTypes::SettlementRun(run)) = decode_record(&run_record)? else {
        return invalid("collect must reference a settlement run");
    };
    let Some(allocation) = run.allocations.get(collect.index as usize) else {
        return invalid("allocation index out of range");
    };
    ensure!(
        allocation.receiver == action.author,
        "only the receiver may collect an allocation"
    );
    ensure!(
        allocation.amounts == collect.amounts,
        "collected amounts must equal the allocation"
    );
    let history = walk_chain(&action.author, &action.prev_action)?;
    ensure!(
        !history
            .collects
            .iter()
            .any(|c| c.run == collect.run && c.index == collect.index),
        "allocation already collected"
    );
    valid()
}

fn validate_debit(action: &Create, debit: Amounts) -> ExternResult<ValidateCallbackResult> {
    let history = walk_chain(&action.author, &action.prev_action)?;
    let available = history.available().map_err(core_err)?;
    ensure!(
        available.covers(&debit),
        "insufficient balance: available {available:?}, required {debit:?}"
    );
    valid()
}

fn validate_create_link(
    link_type: LinkTypes,
    base: AnyLinkableHash,
    target: AnyLinkableHash,
    action: &CreateLink,
) -> ExternResult<ValidateCallbackResult> {
    // Every ledger link's target is a ledger entry authored by the link author.
    let Some(target_hash) = target.into_action_hash() else {
        return invalid("link target must be an action hash");
    };
    let target_record = must_get_valid_record(target_hash)?;
    ensure!(
        target_record.action().author() == &action.author,
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
                base.into_agent_pub_key() == Some(action.author.clone()),
                "base must be the maker"
            );
        }
        _ => return invalid("link target has the wrong entry type"),
    }
    valid()
}

fn core_err(e: CoreError) -> WasmError {
    wasm_error!(WasmErrorInner::Guest(e.to_string()))
}

// ---------------------------------------------------------------------------
// Decoding and chain walking (shared with the coordinator)
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

/// The ledger entries on one agent's source chain.
#[derive(Debug, Default)]
pub struct ChainLedger {
    pub mints: Vec<Mint>,
    pub escrows: Vec<(ActionHash, Escrow)>,
    pub parks: Vec<(ActionHash, Park)>,
    /// `(action hash, action_seq, run)`.
    pub runs: Vec<(ActionHash, u32, SettlementRun)>,
    pub collects: Vec<Collect>,
}

impl ChainLedger {
    /// Funds the agent can still spend: credits minus debits.
    pub fn available(&self) -> Result<Amounts, CoreError> {
        let mut credits = Amounts::ZERO;
        for m in &self.mints {
            credits = credits.checked_add(m.amounts).ok_or(CoreError::Overflow)?;
        }
        for c in &self.collects {
            credits = credits.checked_add(c.amounts).ok_or(CoreError::Overflow)?;
        }
        let mut debits = Amounts::ZERO;
        for (_, e) in &self.escrows {
            debits = debits.checked_add(e.terms.initial_lock()?).ok_or(CoreError::Overflow)?;
        }
        for (_, p) in &self.parks {
            debits = debits.checked_add(p.amounts).ok_or(CoreError::Overflow)?;
        }
        credits.checked_sub(debits).ok_or(CoreError::Overflow)
    }

    pub fn push(&mut self, hash: ActionHash, seq: u32, entry: EntryTypes) {
        match entry {
            EntryTypes::Mint(m) => self.mints.push(m),
            EntryTypes::Escrow(e) => self.escrows.push((hash, e)),
            EntryTypes::Park(p) => self.parks.push((hash, p)),
            EntryTypes::SettlementRun(r) => self.runs.push((hash, seq, r)),
            EntryTypes::Collect(c) => self.collects.push(c),
        }
    }
}

/// Deterministically read every ledger entry on `author`'s chain from
/// `chain_top` back to genesis. Used by validation, so the result is identical
/// on every peer.
pub fn walk_chain(author: &AgentPubKey, chain_top: &ActionHash) -> ExternResult<ChainLedger> {
    let activity = must_get_agent_activity(
        author.clone(),
        ChainFilter::new(chain_top.clone()).include_cached_entries(),
    )?;
    let mut ledger = ChainLedger::default();
    for item in activity {
        let action = &item.action.hashed.content;
        let Some((entry_hash, EntryType::App(_))) = action.entry_data() else {
            continue;
        };
        let entry = match item.cached_entry {
            Some(entry) => entry,
            None => must_get_entry(entry_hash.clone())?.content,
        };
        if let Some(decoded) = decode_entry(action, &entry)? {
            ledger.push(item.action.hashed.hash.clone(), action.action_seq(), decoded);
        }
    }
    Ok(ledger)
}
