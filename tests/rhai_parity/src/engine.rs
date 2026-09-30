//! Running one settlement run both ways, and comparing them.

use dex_core::{execute_run, select_parks, Amounts, CoreError, OrderTerms, ParkInput, RunInput, RunMode, RunOutput, Side, LOT_SIZE_A, MAX_PARKS_PER_RUN};
use hdi::prelude::*;
use hdk::hdk::set_hdk;
use hdk::prelude::MockHdkT;
use rave_engine::prelude::{PresetVariables, RhaiEngine, RhaiEngineConfig, RhaiEngineOutput};
use rave_engine::types::{
    CarryForwardUnits, LinkTypes, ParkedSpendData, RAVEInput, RAVEInputHandler, RAVEInputStdPayload,
    RAVEInputStdPayloadInner, RAVEOutput, UnitMap,
};
use serde_json::{json, Value};
use std::collections::BTreeMap;

pub const TEMPLATE: &str = include_str!("../../../unyt/dex_order_escrow/execution_code.rhai");
pub const UNIT_A: &str = "1";
pub const UNIT_B: &str = "2";

// Agents by index: 0 is the maker (ALICE in dex_core's tests).
pub const ALICE: u8 = 0;
pub const BOB: u8 = 1;
pub const CAROL: u8 = 2;

pub fn agent(i: u8) -> AgentPubKey {
    AgentPubKey::from_raw_32(vec![0x40 + i; 32])
}

pub fn agent_str(i: u8) -> String {
    AgentPubKeyB64::from(agent(i)).to_string()
}

fn hash(tag: u8, n: u32) -> ActionHash {
    let mut bytes = vec![tag; 32];
    bytes[..4].copy_from_slice(&n.to_be_bytes());
    ActionHash::from_raw_32(bytes)
}

pub fn park_id(n: u32) -> String {
    ActionHashB64::from(hash(0x11, n)).to_string()
}

fn ea_id() -> ActionHash {
    hash(0x22, 0)
}

fn maker_spend_id() -> ActionHash {
    hash(0x33, 0)
}

fn prev_run_id() -> ActionHash {
    hash(0x44, 0)
}

#[derive(Clone, Debug)]
pub struct Park {
    pub id: u32,
    pub taker: u8,
    pub amounts: Amounts,
    pub requested_lots: u64,
    pub parked_at: i64,
}

pub fn park(id: u32, taker: u8, amounts: Amounts, requested_lots: u64, parked_at: i64) -> Park {
    Park { id, taker, amounts, requested_lots, parked_at }
}

/// Where a run's lock comes from.
#[derive(Clone, Debug)]
pub enum Start {
    /// The opening run: a maker spend of exactly the initial lock.
    Opening,
    /// A later run: `previous_execution` carries this lock.
    Locked(Amounts),
    /// A later run chained from an actual previous engine output.
    Chained(RAVEOutput),
}

#[derive(Clone, Debug)]
pub struct Case {
    pub terms: OrderTerms,
    pub start: Start,
    /// Pending taker spends, in the order the DNA hands them to the script.
    pub parks: Vec<Park>,
    pub now: i64,
    pub mode: RunMode,
}

// ---------------------------------------------------------------------------
// Amounts <-> unit maps
// ---------------------------------------------------------------------------

/// Minor units -> "123.45", independently of the template's formatter.
pub fn fmt(n: u64) -> String {
    format!("{}.{:02}", n / 100, n % 100)
}

/// "123.45" (any trailing zeros past 2 decimals allowed) -> minor units.
pub fn minor(s: &str) -> u64 {
    let (whole, frac) = s.split_once('.').unwrap_or((s, ""));
    let (keep, extra) = frac.split_at(frac.len().min(2));
    assert!(extra.chars().all(|c| c == '0'), "finer than 2 decimals: {s}");
    whole.parse::<u64>().unwrap_or_else(|_| panic!("bad amount {s}")) * 100
        + format!("{keep:0<2}").parse::<u64>().unwrap()
}

/// A unit map with no zero amounts (a zero on a monetary unit is invalid).
pub fn unit_map_json(a: &Amounts) -> Value {
    let mut m = serde_json::Map::new();
    for (unit, amount) in a.units() {
        m.insert(unyt_index(unit).into(), fmt(amount).into());
    }
    Value::Object(m)
}

/// dex_core's unit ids as the template's Unyt unit indexes.
fn unyt_index(unit: &str) -> &'static str {
    match unit {
        dex_core::UNIT_A => UNIT_A,
        dex_core::HUB_UNIT => UNIT_B,
        other => panic!("no Unyt index for unit {other}"),
    }
}

fn unit_map(a: &Amounts) -> UnitMap {
    serde_json::from_value(unit_map_json(a)).expect("unit map")
}

pub fn amounts_of(map: &UnitMap) -> Amounts {
    let v = serde_json::to_value(map).expect("unit map to json");
    let mut out = Amounts::ZERO;
    for (unit, amount) in v.as_object().expect("unit map is an object") {
        let n = minor(amount.as_str().expect("amount is a string"));
        let id = match unit.as_str() {
            UNIT_A => dex_core::UNIT_A,
            UNIT_B => dex_core::HUB_UNIT,
            other => panic!("unexpected unit {other}"),
        };
        out = out.checked_add(&Amounts::of(id, n)).expect("no overflow");
    }
    out
}

// ---------------------------------------------------------------------------
// The host: parked-link records for acceding_sort_allocation
// ---------------------------------------------------------------------------

fn parked_spend_record(role: &str, author: AgentPubKey, timestamp: i64, amount: Amounts, requested: Option<u64>) -> Record {
    let tag = rmp_serde::to_vec_named(&ParkedSpendData {
        ct_role_id: role.to_string(),
        amount: unit_map(&amount),
        fee: UnitMap::new(),
        payload: requested.map_or(Value::Null, |n| json!({ "requested_lots": n })),
        global_definition: hash(0x55, 0),
        lane_definitions: Vec::new(),
        new_balance: UnitMap::new(),
        carry_forward_units: CarryForwardUnits::default(),
        fees_owed: UnitMap::new(),
        proposed_balance: UnitMap::new(),
    })
    .expect("encode ParkedSpendData");
    let link_type = LinkTypes::iter()
        .position(|t| t == LinkTypes::ParkedSpendBalance)
        .expect("ParkedSpendBalance is a link type") as u8;
    let action: Action = TypedAction {
        header: ActionHeader {
            author,
            timestamp: Timestamp::from_micros(timestamp),
            action_seq: 5,
            prev_action: Some(hash(0x66, 0)),
        },
        data: CreateLinkData {
            base_address: ea_id().into(),
            target_address: agent(ALICE).into(),
            zome_index: 0.into(),
            link_type: link_type.into(),
            tag: LinkTag(tag),
        },
    }
    .into();
    Record::new(
        SignedActionHashed::with_presigned(ActionHashed::from_content_sync(action), Signature([0u8; 64])),
        RecordEntry::NA,
    )
}

fn zome_info() -> ZomeInfo {
    ZomeInfo {
        name: "dex_parity".to_string().into(),
        id: 0u8.into(),
        properties: SerializedBytes::default(),
        entry_defs: EntryDefs(Default::default()),
        extern_fns: Default::default(),
        zome_types: ScopedZomeTypesSet {
            entries: ScopedZomeTypes(vec![(ZomeIndex(0), vec![EntryDefIndex(0)])]),
            links: ScopedZomeTypes(vec![(ZomeIndex(0), (0..LinkTypes::len()).map(LinkType).collect())]),
        },
    }
}

/// Install a host that answers every parked link of `case` (thread-local).
fn install_host(case: &Case) {
    let mut records: BTreeMap<ActionHash, Record> = BTreeMap::new();
    for p in &case.parks {
        records.insert(
            hash(0x11, p.id),
            parked_spend_record("taker_spender", agent(p.taker), p.parked_at, p.amounts.clone(), Some(p.requested_lots)),
        );
    }
    let mut mock = MockHdkT::new();
    mock.expect_must_get_valid_record().returning(move |input| {
        records
            .get(&input.into_inner())
            .cloned()
            .ok_or_else(|| wasm_error!(WasmErrorInner::Guest("no such record".into())))
    });
    mock.expect_zome_info().returning(|_| Ok(zome_info()));
    mock.expect_trace().returning(|_| Ok(()));
    set_hdk(mock);
}

// ---------------------------------------------------------------------------
// Inputs
// ---------------------------------------------------------------------------

fn single(data: Value) -> RAVEInputStdPayload {
    RAVEInputStdPayload::Single(RAVEInputStdPayloadInner { data: Box::new(data), link_hash: None })
}

fn linked(data: Value, link: &ActionHash) -> RAVEInputStdPayloadInner {
    RAVEInputStdPayloadInner { data: Box::new(data), link_hash: Some(link.clone().into()) }
}

fn previous_output(case: &Case) -> Option<Value> {
    match &case.start {
        Start::Opening => None,
        Start::Locked(lock) => {
            let output = RAVEOutput::try_from(json!({ "locked": unit_map_json(lock) })).expect("RAVEOutput");
            Some(serde_json::to_value(output).expect("serialize RAVEOutput"))
        }
        Start::Chained(output) => Some(serde_json::to_value(output).expect("serialize RAVEOutput")),
    }
}

pub fn input_json(case: &Case) -> Value {
    let t = &case.terms;
    let mut consumed = RAVEInputHandler::new();
    if let Start::Opening = case.start {
        let lock = t.initial_lock().expect("valid terms");
        let id = maker_spend_id();
        consumed.insert(
            "maker_spender_allocations".into(),
            RAVEInputStdPayload::Vec(vec![linked(
                json!({ "amount": unit_map_json(&lock), "source": ActionHashB64::from(id.clone()).to_string() }),
                &id,
            )]),
        );
    }
    let spends = case
        .parks
        .iter()
        .map(|p| {
            let h = hash(0x11, p.id);
            linked(json!({ "amount": unit_map_json(&p.amounts), "source": park_id(p.id) }), &h)
        })
        .collect();
    let requests = case
        .parks
        .iter()
        .map(|p| linked(json!(p.requested_lots), &hash(0x11, p.id)))
        .collect();
    consumed.insert("taker_spender_allocations".into(), RAVEInputStdPayload::Vec(spends));
    consumed.insert("requested_lots".into(), RAVEInputStdPayload::Vec(requests));

    let mut inputs = RAVEInputHandler::new();
    let prev = previous_output(case)
        .map(|output| json!({ "id": ActionHashB64::from(prev_run_id()).to_string(), "output": output }))
        .unwrap_or(Value::Null);
    inputs.insert("previous_execution".into(), single(prev));
    inputs.insert("side".into(), single(json!(match t.side { Side::Sell => "Sell", Side::Buy => "Buy" })));
    inputs.insert("price_per_lot".into(), single(json!(t.price_per_lot)));
    inputs.insert("lots".into(), single(json!(t.lots)));
    inputs.insert("expires_at".into(), single(json!(t.expires_at)));
    inputs.insert("lot_size".into(), single(json!(LOT_SIZE_A)));
    inputs.insert("unit_a".into(), single(json!(UNIT_A)));
    inputs.insert("unit_b".into(), single(json!(UNIT_B)));
    inputs.insert("max_parks_per_run".into(), single(json!(MAX_PARKS_PER_RUN)));
    inputs.insert("mode".into(), single(json!(match case.mode { RunMode::Fill => "Fill", RunMode::Release => "Release" })));
    Value::from(&RAVEInput::new(consumed, inputs))
}

fn preset(case: &Case) -> PresetVariables {
    PresetVariables {
        ea_id: ea_id(),
        executor: agent(ALICE),
        executed_timestamp: Timestamp::from_micros(case.now),
    }
}

// ---------------------------------------------------------------------------
// Running
// ---------------------------------------------------------------------------

pub fn run_engine(case: &Case, config: RhaiEngineConfig) -> Result<RhaiEngineOutput, String> {
    install_host(case);
    let code = rmp_serde::to_vec(TEMPLATE).expect("encode script");
    RhaiEngine::with_config(config)
        .execute(&input_json(case), code, Some(preset(case)))
        .map_err(|e| format!("{e:?}"))
}

/// What the Rhai run produced, in dex_core's terms.
#[derive(Debug)]
pub struct RhaiRun {
    pub engine: RhaiEngineOutput,
    /// Non-empty allocations by receiver.
    pub paid: BTreeMap<String, Amounts>,
    pub locked: Amounts,
    pub consumed: Vec<String>,
    pub outcomes: Vec<(String, u64)>,
    pub filled_lots: u64,
    pub rejected: Vec<String>,
}

pub fn run_rhai(case: &Case) -> Result<RhaiRun, String> {
    let engine = run_engine(case, RhaiEngineConfig::default())?;
    let out = &engine.output;
    let mut paid = BTreeMap::new();
    for a in out.unyt_allocation.as_deref().unwrap_or_default() {
        let amounts = amounts_of(&a.amounts);
        let receiver = a.receiver.to_string();
        assert!(!paid.contains_key(&receiver), "one allocation per receiver: {receiver}");
        if amounts != Amounts::ZERO {
            paid.insert(receiver, amounts);
        }
    }
    let cv = out.computed_values.clone().expect("computed_values");
    let strings = |v: &Value| -> Vec<String> {
        v.as_array().unwrap().iter().map(|s| s.as_str().unwrap().to_string()).collect()
    };
    Ok(RhaiRun {
        paid,
        locked: out.locked.as_ref().map(amounts_of).unwrap_or(Amounts::ZERO),
        consumed: strings(&cv["consumed"]),
        outcomes: cv["outcomes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|o| (o["park"].as_str().unwrap().to_string(), o["filled_lots"].as_u64().unwrap()))
            .collect(),
        filled_lots: cv["filled_lots"].as_u64().unwrap(),
        rejected: engine.rejected_links.iter().map(|l| l.hash.to_string()).collect(),
        engine,
    })
}

pub fn prev_locked(case: &Case) -> Amounts {
    match &case.start {
        Start::Opening => case.terms.initial_lock().expect("valid terms"),
        Start::Locked(lock) => lock.clone(),
        Start::Chained(output) => output.locked.as_ref().map(amounts_of).unwrap_or(Amounts::ZERO),
    }
}

/// dex_core on the same case: the ledger coordinator's select_parks, then
/// execute_run, with string ids and keys so ties break the same way.
pub fn run_core(case: &Case) -> (Result<RunOutput<String, String>, CoreError>, Vec<String>) {
    let pending: Vec<ParkInput<String, String>> = case
        .parks
        .iter()
        .map(|p| ParkInput {
            id: park_id(p.id),
            taker: agent_str(p.taker),
            amounts: p.amounts.clone(),
            requested_lots: p.requested_lots,
            parked_at: p.parked_at,
        })
        .collect();
    let mut sorted = pending.clone();
    sorted.sort_by(|x, y| (x.parked_at, &x.id).cmp(&(y.parked_at, &y.id)));
    let deferred = sorted.iter().skip(MAX_PARKS_PER_RUN).map(|p| p.id.clone()).collect();
    let result = execute_run(&RunInput {
        terms: case.terms,
        maker: agent_str(ALICE),
        prev_locked: prev_locked(case),
        parks: select_parks(pending),
        now: case.now,
        mode: case.mode,
    });
    (result, deferred)
}

// ---------------------------------------------------------------------------
// Comparing
// ---------------------------------------------------------------------------

/// Run both, require the same verdict and, on success, the same run; check
/// conservation per unit and source naming on the Rhai output.
pub fn assert_parity(case: &Case) -> Option<RhaiRun> {
    let rhai = run_rhai(case);
    let (core, deferred) = run_core(case);
    match (rhai, core) {
        (Err(_), Err(_)) => None,
        (Ok(r), Err(e)) => panic!("dex_core refused ({e}) but Rhai ran: {r:?}\ncase: {case:?}"),
        (Err(e), Ok(c)) => panic!("Rhai refused but dex_core ran: {e}\ncore: {c:?}\ncase: {case:?}"),
        (Ok(r), Ok(c)) => {
            let core_paid: BTreeMap<String, Amounts> =
                c.allocations.iter().map(|a| (a.receiver.clone(), a.amounts.clone())).collect();
            assert_eq!(r.paid, core_paid, "allocations differ\ncase: {case:?}");
            assert_eq!(r.locked, c.locked, "locked differs\ncase: {case:?}");
            assert_eq!(r.consumed, c.consumed, "consumed order differs\ncase: {case:?}");
            let core_outcomes: Vec<(String, u64)> =
                c.outcomes.iter().map(|o| (o.id.clone(), o.filled_lots)).collect();
            assert_eq!(r.outcomes, core_outcomes, "fills differ\ncase: {case:?}");
            assert_eq!(r.filled_lots, c.filled_lots);
            assert_eq!(r.rejected, deferred, "deferred parks differ\ncase: {case:?}");
            assert_conserves(case, &r);
            assert_names_sources(case, &r);
            Some(r)
        }
    }
}

/// Per unit: paid + locked == consumed parks + previous lock.
pub fn assert_conserves(case: &Case, r: &RhaiRun) {
    let mut inflow = prev_locked(case);
    for id in &r.consumed {
        let p = case.parks.iter().find(|p| &park_id(p.id) == id).expect("consumed park exists");
        inflow = inflow.checked_add(&p.amounts).unwrap();
    }
    let mut outflow = r.locked.clone();
    for a in r.paid.values() {
        outflow = outflow.checked_add(a).unwrap();
    }
    assert_eq!(inflow, outflow, "conservation per unit\ncase: {case:?}");
}

/// Every consumed spend is named, deferred ones are not, the opening spend is
/// named, and an allocation drawing on the lock names it.
pub fn assert_names_sources(case: &Case, r: &RhaiRun) {
    let allocations = r.engine.output.unyt_allocation.clone().unwrap_or_default();
    let named: Vec<String> = allocations.iter().flat_map(|a| a.sources.iter().map(|s| s.to_string())).collect();
    for id in &r.consumed {
        assert!(named.contains(id), "consumed park {id} is not named\ncase: {case:?}");
    }
    for id in &r.rejected {
        assert!(!named.contains(id), "deferred park {id} is named\ncase: {case:?}");
    }
    let lock_source = match case.start {
        Start::Opening => ActionHashB64::from(maker_spend_id()).to_string(),
        _ => ActionHashB64::from(prev_run_id()).to_string(),
    };
    if let Start::Opening = case.start {
        assert!(named.contains(&lock_source), "the opening spend is not named");
    }
    let in_maker_unit = |x: Amounts| match case.terms.side {
        Side::Sell => x.get(dex_core::UNIT_A),
        Side::Buy => x.get(dex_core::HUB_UNIT),
    };
    for a in &allocations {
        let paid = in_maker_unit(amounts_of(&a.amounts));
        // What the receiver's own named spends refund in the maker asset.
        let own_refund: u64 = case
            .parks
            .iter()
            .filter(|p| a.sources.iter().any(|s| s.to_string() == park_id(p.id)))
            .filter(|p| agent_str(p.taker) == a.receiver.to_string())
            .map(|p| in_maker_unit(p.amounts.clone()))
            .sum();
        if paid > own_refund {
            assert!(
                a.sources.iter().any(|s| s.to_string() == lock_source),
                "an allocation paying the maker asset does not name the lock\ncase: {case:?}"
            );
        }
    }
}
