//! Crafted-chain tests of the run-or-reclaim rule (design doc section 9,
//! layer 2).
//!
//! An honest conductor stamps every action `max(now, head + 1µs)`, so a
//! Sweettest cannot write a backdated action. Here chains are built by hand
//! with any timestamps system validation allows (non-decreasing along a
//! chain, `sys_validate.rs:247`), and the real `ledger_integrity::validate` is
//! called natively. `#[hdk_extern]` leaves it an ordinary function. A fake DHT
//! answers its host calls (`must_get_valid_record`, `must_get_agent_activity`,
//! `dna_info`, `zome_info`).
//!
//! Each trace T1–T13 ends with **exactly one** of run or Reclaim valid, and
//! supply conserved (the park's funds counted once). The one exception is T12,
//! a fork, where both are valid on their own branches. Holochain warrants the
//! fork; validation cannot see across branches.

use dex_core::properties::{DexProperties, UnitDef};
use dex_core::{execute_run, Amounts, MarketDef, OrderTerms, ParkInput, RunInput, RunMode, Side, HUB_UNIT, UNIT_A};
use hdi::prelude::*;
use ledger_integrity::{
    validate, Collect, Escrow, EntryTypes, LinkTypes, Mint, Park, Reclaim, SettlementRun, UnitEntryTypes,
};
use std::collections::{BTreeMap, HashMap};

// ---------------------------------------------------------------------------
// A fake DHT and hand-built chains
// ---------------------------------------------------------------------------

const SEC: i64 = 1_000_000;

/// The test DNA's timing: 20 s to settle a park, then 5 s grace.
fn props() -> DexProperties {
    DexProperties {
        park_timeout_secs: 20,
        settle_grace_secs: 5,
        units: vec![UnitDef { id: UNIT_A.into(), decimals: 2 }, UnitDef { id: HUB_UNIT.into(), decimals: 2 }],
        markets: vec![MarketDef::default_pair()],
    }
}

#[derive(Clone, Default)]
struct Dht {
    records: HashMap<ActionHash, Record>,
    properties: Vec<u8>,
}

fn not_found(what: impl std::fmt::Debug) -> WasmError {
    wasm_error!(WasmErrorInner::Host(format!("fake DHT: {what:?} not found")))
}

impl HdiT for Dht {
    fn verify_signature(&self, _: VerifySignature) -> ExternResult<bool> {
        Ok(true)
    }
    fn must_get_entry(&self, input: MustGetEntryInput) -> ExternResult<EntryHashed> {
        self.records
            .values()
            .find_map(|r| {
                let entry = r.entry().as_option()?;
                (r.action().entry_hash() == Some(&input.0)).then(|| EntryHashed::with_pre_hashed(entry.clone(), input.0.clone()))
            })
            .ok_or_else(|| not_found(&input.0))
    }
    fn must_get_action(&self, input: MustGetActionInput) -> ExternResult<SignedActionHashed> {
        self.records.get(&input.0).map(|r| r.signed_action().clone()).ok_or_else(|| not_found(&input.0))
    }
    fn must_get_valid_record(&self, input: MustGetValidRecordInput) -> ExternResult<Record> {
        self.records.get(&input.0).cloned().ok_or_else(|| not_found(&input.0))
    }
    /// Follows `prev_action` from the top, down to and including the
    /// `until_hash` bottom (or genesis), as the host does.
    fn must_get_agent_activity(&self, input: MustGetAgentActivityInput) -> ExternResult<Vec<AgentActivity>> {
        let filter = input.chain_filter;
        let until = match &filter.limit_conditions {
            LimitConditions::UntilHash(h) => Some(h.clone()),
            LimitConditions::ToGenesis => None,
            other => panic!("fake DHT does not implement {other:?}"),
        };
        let mut out = Vec::new();
        let mut next = Some(filter.chain_top.clone());
        while let Some(hash) = next {
            let record = self.records.get(&hash).ok_or_else(|| not_found(&hash))?;
            assert_eq!(record.action().author(), &input.author, "walk left the author's chain");
            out.push(AgentActivity {
                action: record.signed_action().clone(),
                cached_entry: filter.include_cached_entries.then(|| record.entry().as_option().cloned()).flatten(),
            });
            if until.as_ref() == Some(&hash) {
                break;
            }
            next = record.action().prev_action().cloned();
        }
        if let Some(bottom) = until {
            assert!(out.iter().any(|a| a.action.hashed.hash == bottom), "until_hash {bottom} not on the chain");
        }
        Ok(out)
    }
    fn dna_info(&self, _: ()) -> ExternResult<DnaInfo> {
        Ok(DnaInfoV2 {
            name: "dex".into(),
            hash: DnaHash::from_raw_36(vec![0xdd; 36]),
            modifiers: DnaModifiers {
                network_seed: String::new(),
                properties: SerializedBytes::from(UnsafeBytes::from(self.properties.clone())),
            },
            zome_names: vec!["ledger_integrity".into()],
        })
    }
    fn zome_info(&self, _: ()) -> ExternResult<ZomeInfo> {
        let entries = (0..7).map(EntryDefIndex).collect();
        let links = (0..5).map(LinkType).collect();
        Ok(ZomeInfo::new(
            "ledger_integrity".into(),
            ZomeIndex(0),
            SerializedBytes::default(),
            EntryDefs(Vec::new()),
            Vec::new(),
            ScopedZomeTypesSet {
                entries: ScopedZomeTypes(vec![(ZomeIndex(0), entries)]),
                links: ScopedZomeTypes(vec![(ZomeIndex(0), links)]),
            },
        ))
    }
    fn trace(&self, _: TraceMsg) -> ExternResult<()> {
        Ok(())
    }
    fn x_salsa20_poly1305_decrypt(&self, _: XSalsa20Poly1305Decrypt) -> ExternResult<Option<XSalsa20Poly1305Data>> {
        unimplemented!()
    }
    fn x_25519_x_salsa20_poly1305_decrypt(
        &self,
        _: X25519XSalsa20Poly1305Decrypt,
    ) -> ExternResult<Option<XSalsa20Poly1305Data>> {
        unimplemented!()
    }
    fn ed_25519_x_salsa20_poly1305_decrypt(
        &self,
        _: Ed25519XSalsa20Poly1305Decrypt,
    ) -> ExternResult<XSalsa20Poly1305Data> {
        unimplemented!()
    }
}

/// One agent's chain head.
#[derive(Clone)]
struct Chain {
    author: AgentPubKey,
    head: ActionHash,
    seq: u32,
    ts: i64,
}

/// Hand-built chains over one fake DHT. Hashes are counters, not content
/// hashes: validation compares them, never recomputes them.
struct World {
    dht: Dht,
    next: u32,
}

fn hash36(tag: u8, n: u32) -> Vec<u8> {
    let mut raw = vec![tag; 36];
    raw[..4].copy_from_slice(&n.to_be_bytes());
    raw
}

impl World {
    fn new() -> Self {
        let properties = holochain_serialized_bytes::encode(&props()).expect("properties encode");
        World { dht: Dht { records: HashMap::new(), properties }, next: 1 }
    }

    fn fresh(&mut self) -> u32 {
        self.next += 1;
        self.next
    }

    /// A new agent: a chain holding only its `Dna` action, at `ts`.
    fn agent(&mut self, id: u8, ts: i64) -> Chain {
        let author = AgentPubKey::from_raw_36(vec![id; 36]);
        let n = self.fresh();
        let hash = ActionHash::from_raw_36(hash36(0xa0, n));
        let action = Action {
            header: ActionHeader { author: author.clone(), timestamp: Timestamp(ts), action_seq: 0, prev_action: None },
            data: ActionData::Dna(DnaData { dna_hash: DnaHash::from_raw_36(vec![0xdd; 36]) }),
        };
        let record = Record::new(signed(action, hash.clone()), RecordEntry::NA);
        self.dht.records.insert(hash.clone(), record);
        Chain { author, head: hash, seq: 0, ts }
    }

    /// The record `chain` would get for `entry` at `ts`. Not stored and the
    /// chain not advanced: see [`World::commit`]. A timestamp before the
    /// chain head is allowed here on purpose, to show what system
    /// validation (not app validation) refuses.
    fn draft(&mut self, chain: &Chain, ts: i64, entry: &EntryTypes) -> Record {
        let n = self.fresh();
        let hash = ActionHash::from_raw_36(hash36(0xa1, n));
        let entry_hash = EntryHash::from_raw_36(hash36(0xe1, n));
        let unit: UnitEntryTypes = entry.to_unit();
        let action = Action {
            header: ActionHeader {
                author: chain.author.clone(),
                timestamp: Timestamp(ts),
                action_seq: chain.seq + 1,
                prev_action: Some(chain.head.clone()),
            },
            data: ActionData::Create(CreateData {
                entry_type: EntryType::App(AppEntryDef {
                    entry_index: EntryDefIndex(unit as u8),
                    zome_index: ZomeIndex(0),
                    visibility: EntryVisibility::Public,
                }),
                entry_hash,
            }),
        };
        let entry = Entry::try_from(entry).expect("entry serializes");
        Record::new(signed(action, hash), RecordEntry::Present(entry))
    }

    /// Store `record` and make it `chain`'s head.
    fn commit(&mut self, chain: &mut Chain, record: Record) -> ActionHash {
        let hash = record.action_address().clone();
        chain.head = hash.clone();
        chain.seq = record.action().action_seq();
        chain.ts = record.action().timestamp().as_micros();
        self.dht.records.insert(hash.clone(), record);
        hash
    }

    /// Validate `record` against the DHT as it stands.
    fn verdict(&self, record: &Record) -> ValidateCallbackResult {
        set_hdi(self.dht.clone());
        validate(Op::CreateRecord(CreateRecord { record: record.clone() })).expect("validate returned an error")
    }

    /// Draft, require validation to pass, and commit.
    fn write(&mut self, chain: &mut Chain, ts: i64, entry: EntryTypes) -> ActionHash {
        assert!(ts >= chain.ts, "an honest write never goes back in time");
        let record = self.draft(chain, ts, &entry);
        assert_eq!(self.verdict(&record), ValidateCallbackResult::Valid, "setup write {entry:?} must be valid");
        self.commit(chain, record)
    }

    fn record(&self, hash: &ActionHash) -> &Record {
        &self.dht.records[hash]
    }

    fn entry(&self, hash: &ActionHash) -> EntryTypes {
        ledger_integrity::decode_record(self.record(hash)).expect("decodes").expect("is a ledger entry")
    }
}

fn signed(action: Action, hash: ActionHash) -> SignedActionHashed {
    SignedHashed { hashed: HoloHashed::with_pre_hashed(action, hash), signature: Signature([0; 64]) }
}

fn is_valid(v: &ValidateCallbackResult) -> bool {
    matches!(v, ValidateCallbackResult::Valid)
}

fn assert_invalid(v: &ValidateCallbackResult, because: &str) {
    match v {
        ValidateCallbackResult::Invalid(why) => assert!(why.contains(because), "invalid for {why:?}, expected {because:?}"),
        other => panic!("expected invalid ({because}), got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// The scenario every trace starts from
// ---------------------------------------------------------------------------

/// Maker M sells 100 lots of A at 1.20 HF; taker T parks 48 HF for 40 lots
/// at `park_ts`. Deadline D = park_ts + 20 s + 5 s (the order expires much
/// later).
struct Scene {
    w: World,
    maker: Chain,
    taker: Chain,
    escrow: ActionHash,
    park: ActionHash,
    deadline: i64,
    minted: Amounts,
}

const T0: i64 = 1_000 * SEC;

fn scene_with_park_at(park_ts: i64) -> Scene {
    let mut w = World::new();
    let mut maker = w.agent(1, T0);
    let mut taker = w.agent(2, T0);
    w.write(&mut maker, T0 + SEC, EntryTypes::Mint(Mint { amounts: Amounts::new(10_000, 0) }));
    w.write(&mut taker, T0 + SEC, EntryTypes::Mint(Mint { amounts: Amounts::new(0, 12_000) }));
    let terms = OrderTerms { side: Side::Sell, price_per_lot: 120, lots: 100, expires_at: T0 + 3_600 * SEC };
    let escrow = w.write(
        &mut maker,
        T0 + 2 * SEC,
        EntryTypes::Escrow(Escrow { market: MarketDef::default_pair().id(), terms, checkpoint: None }),
    );
    let park = w.write(
        &mut taker,
        park_ts,
        EntryTypes::Park(Park {
            escrow: escrow.clone(),
            market: MarketDef::default_pair().id(),
            amounts: Amounts::new(0, 4_800),
            requested_lots: 40,
            checkpoint: None,
        }),
    );
    let deadline = park_ts + 25 * SEC;
    Scene { w, maker, taker, escrow, park, deadline, minted: Amounts::new(10_000, 12_000) }
}

fn scene() -> Scene {
    scene_with_park_at(T0 + 10 * SEC)
}

impl Scene {
    /// The run the maker's coordinator would write at `ts`, consuming
    /// `parks` (through `dex_core::execute_run`, like the coordinator).
    fn run_entry(&self, ts: i64, parks: &[ActionHash]) -> EntryTypes {
        let EntryTypes::Escrow(escrow) = self.w.entry(&self.escrow) else { unreachable!() };
        let market = MarketDef::default_pair();
        let parks = parks
            .iter()
            .map(|p| {
                let EntryTypes::Park(park) = self.w.entry(p) else { panic!("not a park") };
                let action = self.w.record(p).action();
                ParkInput {
                    id: p.clone(),
                    taker: action.author().clone(),
                    amounts: park.amounts,
                    requested_lots: park.requested_lots,
                    parked_at: action.timestamp().as_micros(),
                }
            })
            .collect();
        let input = RunInput {
            terms: escrow.terms,
            market: market.clone(),
            maker: self.maker.author.clone(),
            prev_locked: escrow.terms.initial_lock(&market).expect("lock"),
            parks,
            now: ts,
            mode: RunMode::Fill,
        };
        let out = execute_run(&input).expect("settlement logic");
        EntryTypes::SettlementRun(SettlementRun {
            escrow: self.escrow.clone(),
            prev_run: None,
            mode: RunMode::Fill,
            consumed: out.consumed,
            allocations: out.allocations,
            locked: out.locked,
        })
    }

    /// Any maker action: here a mint, as a trade or a collect would be.
    fn maker_acts(&mut self, ts: i64) -> ActionHash {
        let mut maker = self.maker.clone();
        let hash = self.w.write(&mut maker, ts, EntryTypes::Mint(Mint { amounts: Amounts::new(1, 0) }));
        self.maker = maker;
        self.minted = self.minted.checked_add(&Amounts::new(1, 0)).expect("no overflow");
        hash
    }

    /// Draft the maker's run at `ts` and return (record, verdict).
    fn try_run(&mut self, ts: i64) -> (Record, ValidateCallbackResult) {
        let entry = self.run_entry(ts, std::slice::from_ref(&self.park));
        let record = self.w.draft(&self.maker, ts, &entry);
        let verdict = self.w.verdict(&record);
        (record, verdict)
    }

    /// Draft `by`'s Reclaim of `park` citing `anchor` at `ts`.
    fn try_reclaim_by(&mut self, by: &Chain, park: &ActionHash, anchor: &ActionHash, ts: i64) -> (Record, ValidateCallbackResult) {
        let entry = EntryTypes::Reclaim(Reclaim { park: park.clone(), anchor: anchor.clone() });
        let record = self.w.draft(by, ts, &entry);
        let verdict = self.w.verdict(&record);
        (record, verdict)
    }

    fn try_reclaim(&mut self, anchor: &ActionHash, ts: i64) -> (Record, ValidateCallbackResult) {
        let taker = self.taker.clone();
        let park = self.park.clone();
        self.try_reclaim_by(&taker, &park, anchor, ts)
    }

    fn commit_maker(&mut self, record: Record) -> ActionHash {
        let mut maker = self.maker.clone();
        let hash = self.w.commit(&mut maker, record);
        self.maker = maker;
        hash
    }

    fn commit_taker(&mut self, record: Record) -> ActionHash {
        let mut taker = self.taker.clone();
        let hash = self.w.commit(&mut taker, record);
        self.taker = taker;
        hash
    }

    /// Supply held, over every committed (so valid) entry: each chain's
    /// available balance, the escrow's current lock, unsettled parks, and
    /// uncollected allocations. Must equal what was minted: a park counted by
    /// both a run and a Reclaim would show here as extra supply.
    fn assert_supply_conserved(&self) {
        let mut held = Amounts::ZERO;
        let add = |held: &mut Amounts, x: &Amounts| *held = held.checked_add(x).expect("no overflow");
        let mut consumed = Vec::new();
        let mut reclaimed = Vec::new();
        let mut lock: BTreeMap<ActionHash, Amounts> = BTreeMap::new();
        let mut runs = Vec::new();
        for record in self.w.dht.records.values() {
            let Some(entry) = ledger_integrity::decode_record(record).expect("decodes") else { continue };
            match entry {
                EntryTypes::Mint(m) => add(&mut held, &m.amounts),
                EntryTypes::SettlementRun(r) => {
                    consumed.extend(r.consumed.iter().cloned());
                    lock.insert(r.escrow.clone(), r.locked.clone());
                    runs.push(r);
                }
                EntryTypes::Reclaim(r) => reclaimed.push(r.park),
                _ => {}
            }
        }
        // Σ holdings = Σ available + Σ locks + Σ open parks + Σ uncollected,
        // and available = minted + collected + reclaimed − escrowed − parked.
        // Collected + uncollected is every allocation; reclaimed + open is
        // every unsettled park. So Σ holdings = minted − escrowed − parked
        //   + current locks + unsettled parks + every allocation.
        let mut escrowed = Amounts::ZERO;
        let mut parked = Amounts::ZERO;
        let mut back = Amounts::ZERO;
        for (hash, record) in &self.w.dht.records {
            match ledger_integrity::decode_record(record).expect("decodes") {
                Some(EntryTypes::Escrow(e)) => {
                    let initial = e.terms.initial_lock(&MarketDef::default_pair()).expect("lock");
                    add(&mut escrowed, &initial);
                    add(&mut back, lock.get(hash).unwrap_or(&initial));
                }
                Some(EntryTypes::Park(p)) => {
                    add(&mut parked, &p.amounts);
                    let settled = consumed.contains(hash);
                    let reclaim = reclaimed.iter().filter(|r| *r == hash).count();
                    assert!(!(settled && reclaim > 0), "park {hash} both settled and reclaimed");
                    assert!(reclaim <= 1, "park {hash} reclaimed twice");
                    if !settled {
                        add(&mut back, &p.amounts); // unsettled, or reclaimed into available
                    }
                }
                _ => {}
            }
        }
        for a in runs.iter().flat_map(|r| &r.allocations) {
            add(&mut back, &a.amounts);
        }
        let net = held.checked_add(&back).and_then(|x| x.checked_sub(&escrowed)).and_then(|x| x.checked_sub(&parked));
        assert_eq!(net.as_ref(), Some(&self.minted), "supply must equal what was minted");
    }
}

/// The supply check is only worth something if it fails on a double count:
/// commit both the run and a Reclaim of the same park, bypassing validation.
#[test]
#[should_panic(expected = "both settled and reclaimed")]
fn supply_check_catches_a_park_counted_twice() {
    let mut s = scene();
    let (run, _) = s.try_run(s.deadline - SEC);
    s.commit_maker(run);
    let anchor = s.maker_acts(s.deadline + SEC);
    let (reclaim, verdict) = s.try_reclaim(&anchor, s.deadline + 2 * SEC);
    assert!(!is_valid(&verdict));
    s.commit_taker(reclaim); // what a validator must never accept
    s.assert_supply_conserved();
}

// ---------------------------------------------------------------------------
// The attack traces (design doc section 4.2)
// ---------------------------------------------------------------------------

/// T1: the maker settles in time, then acts after D. The Reclaim's walk finds
/// the run.
#[test]
fn t1_maker_settles_in_time() {
    let mut s = scene();
    let (run, verdict) = s.try_run(s.deadline - 10 * SEC);
    assert!(is_valid(&verdict), "{verdict:?}");
    s.commit_maker(run);
    let anchor = s.maker_acts(s.deadline + SEC);
    let (_, reclaim) = s.try_reclaim(&anchor, s.deadline + 2 * SEC);
    assert_invalid(&reclaim, "consumed");
    s.assert_supply_conserved();
}

/// T2: the maker is back after D but never settles. The Reclaim is valid, and
/// a run after it cannot consume the park.
#[test]
fn t2_maker_back_but_never_settles() {
    let mut s = scene();
    let anchor = s.maker_acts(s.deadline + SEC);
    let (reclaim, verdict) = s.try_reclaim(&anchor, s.deadline + 2 * SEC);
    assert!(is_valid(&verdict), "{verdict:?}");
    s.commit_taker(reclaim);
    let (_, run) = s.try_run(s.deadline + 3 * SEC);
    assert_invalid(&run, "past its deadline");
    s.assert_supply_conserved();
}

/// T3: the maker backdates a run written after the anchor, as far as system
/// validation allows: to the anchor's own timestamp. Still `>= D`, so
/// invalid. A run stamped before the anchor would be refused by system
/// validation (`check_prev_timestamp`), not by this zome; app validation
/// alone would pass it (it reads only the run's own timestamp). That is
/// exactly why the Reclaim walks the maker's chain rather than trusting any
/// run's timestamp.
#[test]
fn t3_backdated_run_after_the_anchor() {
    let mut s = scene();
    let anchor = s.maker_acts(s.deadline + SEC);
    let (reclaim, verdict) = s.try_reclaim(&anchor, s.deadline + 2 * SEC);
    assert!(is_valid(&verdict), "{verdict:?}");
    s.commit_taker(reclaim);

    let as_far_back_as_allowed = s.maker.ts;
    let (_, run) = s.try_run(as_far_back_as_allowed);
    assert_invalid(&run, "past its deadline");

    // Further back breaks the chain's timestamp order: system validation's job.
    let (illegal, _) = s.try_run(s.deadline - SEC);
    assert!(
        illegal.action().timestamp() < s.w.record(&anchor).action().timestamp(),
        "this run would fail check_prev_timestamp"
    );
    s.assert_supply_conserved();
}

/// The 4.1 attack against 4.2: the maker's last action is the escrow; years
/// later they write a run backdated to just after it. The run is valid (it
/// claims a time before D), and so no Reclaim can ever be: any anchor comes
/// after the run on the maker's chain.
#[test]
fn maker_backdates_a_run_to_just_after_their_last_action() {
    let mut s = scene();
    let backdated = s.maker.ts + 1; // the escrow's timestamp + 1µs
    let (run, verdict) = s.try_run(backdated);
    assert!(is_valid(&verdict), "{verdict:?}");
    s.commit_maker(run);
    let years_later = s.maker_acts(T0 + 3 * 365 * 86_400 * SEC);
    let (_, reclaim) = s.try_reclaim(&years_later, T0 + 3 * 365 * 86_400 * SEC + SEC);
    assert_invalid(&reclaim, "consumed");
    s.assert_supply_conserved();
}

/// T4: the maker races the deadline: a run at D − 1µs, the anchor at D.
#[test]
fn t4_run_one_microsecond_before_the_deadline() {
    let mut s = scene();
    let (run, verdict) = s.try_run(s.deadline - 1);
    assert!(is_valid(&verdict), "{verdict:?}");
    s.commit_maker(run);
    let anchor = s.maker_acts(s.deadline);
    let (_, reclaim) = s.try_reclaim(&anchor, s.deadline + SEC);
    assert_invalid(&reclaim, "consumed");
    s.assert_supply_conserved();

    // And a run at exactly D is too late.
    let mut s = scene();
    let (_, late) = s.try_run(s.deadline);
    assert_invalid(&late, "past its deadline");
}

/// T5: the taker reclaims early, citing a maker action before D. The maker
/// can still settle.
#[test]
fn t5_early_reclaim() {
    let mut s = scene();
    let early = s.maker_acts(s.deadline - 2 * SEC);
    let (_, reclaim) = s.try_reclaim(&early, s.deadline + SEC);
    assert_invalid(&reclaim, "deadline");
    let (run, verdict) = s.try_run(s.deadline - SEC);
    assert!(is_valid(&verdict), "{verdict:?}");
    s.commit_maker(run);
    s.assert_supply_conserved();
}

/// T6: the anchor is not on the maker's chain (the taker's own action).
#[test]
fn t6_anchor_by_someone_else() {
    let mut s = scene();
    let own = s.park.clone();
    let (_, reclaim) = s.try_reclaim(&own, s.deadline + SEC);
    assert_invalid(&reclaim, "maker's chain");
    let (run, verdict) = s.try_run(s.deadline - SEC);
    assert!(is_valid(&verdict), "{verdict:?}");
    s.commit_maker(run);
    s.assert_supply_conserved();
}

/// T7: the taker reclaims twice.
#[test]
fn t7_double_reclaim() {
    let mut s = scene();
    let anchor = s.maker_acts(s.deadline + SEC);
    let (first, verdict) = s.try_reclaim(&anchor, s.deadline + 2 * SEC);
    assert!(is_valid(&verdict), "{verdict:?}");
    s.commit_taker(first);
    let (_, second) = s.try_reclaim(&anchor, s.deadline + 3 * SEC);
    assert_invalid(&second, "already reclaimed");
    s.assert_supply_conserved();
}

/// T8: a third party reclaims the taker's park.
#[test]
fn t8_third_party_reclaim() {
    let mut s = scene();
    let anchor = s.maker_acts(s.deadline + SEC);
    let mut thief = s.w.agent(3, T0);
    s.w.write(&mut thief, T0 + SEC, EntryTypes::Mint(Mint { amounts: Amounts::new(0, 1) }));
    s.minted = s.minted.checked_add(&Amounts::new(0, 1)).expect("no overflow");
    let park = s.park.clone();
    let (_, stolen) = s.try_reclaim_by(&thief, &park, &anchor, s.deadline + 2 * SEC);
    assert_invalid(&stolen, "park's author");
    let (own, verdict) = s.try_reclaim(&anchor, s.deadline + 2 * SEC);
    assert!(is_valid(&verdict), "{verdict:?}");
    s.commit_taker(own);
    s.assert_supply_conserved();
}

/// T9: the taker backdates their park (to their own last action, as far as
/// allowed). D moves earlier, which only hurts the taker: the park stops
/// being consumable sooner.
#[test]
fn t9_taker_backdates_the_park() {
    let honest = scene();
    let mut s = scene_with_park_at(T0 + SEC); // the taker's mint time: as far back as allowed
    assert!(s.deadline < honest.deadline);
    let (_, run) = s.try_run(honest.deadline - SEC); // in time for an honest park
    assert_invalid(&run, "past its deadline");
    let anchor = s.maker_acts(honest.deadline);
    let (reclaim, verdict) = s.try_reclaim(&anchor, honest.deadline + SEC);
    assert!(is_valid(&verdict), "{verdict:?}");
    s.commit_taker(reclaim);
    s.assert_supply_conserved();
}

/// T10: an honest maker whose clock is behind stamps the run early; their
/// later actions still anchor after it.
#[test]
fn t10_maker_clock_behind() {
    let mut s = scene();
    let (run, verdict) = s.try_run(s.deadline - 20 * SEC); // "really" after D
    assert!(is_valid(&verdict), "{verdict:?}");
    s.commit_maker(run);
    let anchor = s.maker_acts(s.deadline + 30 * SEC);
    let (_, reclaim) = s.try_reclaim(&anchor, s.deadline + 31 * SEC);
    assert_invalid(&reclaim, "consumed");
    s.assert_supply_conserved();
}

/// T11: an honest maker whose clock is ahead: their action is past D sooner,
/// so the park is reclaimable sooner and no later run can consume it.
#[test]
fn t11_maker_clock_ahead() {
    let mut s = scene();
    let anchor = s.maker_acts(s.deadline); // "really" before D
    let (reclaim, verdict) = s.try_reclaim(&anchor, s.deadline);
    assert!(is_valid(&verdict), "{verdict:?}");
    s.commit_taker(reclaim);
    let (_, run) = s.try_run(s.deadline + SEC);
    assert_invalid(&run, "past its deadline");
    s.assert_supply_conserved();
}

/// T12, documented rather than defended: the maker forks. On one branch a
/// run before D; on another, from the same point, an anchor after D that a
/// Reclaim cites. Each op is valid on its own branch. Validation reads one
/// branch per walk, so this is the one case where both are valid. Holochain
/// detects the fork and warrants the maker (`ChainIntegrityWarrant::ChainFork`).
#[test]
fn t12_fork_makes_both_valid_on_their_branches() {
    let mut s = scene();
    let fork_point = s.maker.clone();
    let (run, verdict) = s.try_run(s.deadline - SEC);
    assert!(is_valid(&verdict), "{verdict:?}");
    s.commit_maker(run);

    s.maker = fork_point; // the other branch starts from the same action
    let anchor = s.maker_acts(s.deadline + SEC);
    let (_, reclaim) = s.try_reclaim(&anchor, s.deadline + 2 * SEC);
    assert!(is_valid(&reclaim), "valid on its branch: {reclaim:?}");
}

/// T13: the maker never writes again. Nothing on their chain is at or after
/// D, so no Reclaim can be valid: the documented liveness limit.
#[test]
fn t13_maker_offline_forever() {
    let mut s = scene();
    let last = s.maker.head.clone(); // the escrow
    let (_, reclaim) = s.try_reclaim(&last, s.deadline + 3_600 * SEC);
    assert_invalid(&reclaim, "deadline");
    s.assert_supply_conserved();
}

/// A settled park's allocation is collected once: a second Collect of it is
/// invalid, so the taker's side of a run is counted once too.
#[test]
fn collect_of_a_settled_park_is_counted_once() {
    let mut s = scene();
    let (run, verdict) = s.try_run(s.deadline - SEC);
    assert!(is_valid(&verdict), "{verdict:?}");
    let run = s.commit_maker(run);
    let EntryTypes::SettlementRun(r) = s.w.entry(&run) else { unreachable!() };
    let index = r.allocations.iter().position(|a| a.receiver == s.taker.author).expect("taker allocation") as u32;
    let amounts = r.allocations[index as usize].amounts.clone();
    let collect = || EntryTypes::Collect(Collect { run: run.clone(), index, amounts: amounts.clone(), checkpoint: None });
    let mut taker = s.taker.clone();
    s.w.write(&mut taker, s.deadline, collect());
    let again = s.w.draft(&taker, s.deadline + SEC, &collect());
    assert_invalid(&s.w.verdict(&again), "already collected");
    s.taker = taker;
    s.assert_supply_conserved();
}

/// The link from an escrow to its reclaims must point at a reclaim of a park
/// of that escrow (a stray link could hide parks from the maker's reads).
#[test]
fn reclaim_link_must_hang_off_the_parks_escrow() {
    let mut s = scene();
    let anchor = s.maker_acts(s.deadline + SEC);
    let (reclaim, _) = s.try_reclaim(&anchor, s.deadline + SEC);
    let reclaim = s.commit_taker(reclaim);
    let link = |base: AnyLinkableHash, taker: &Chain, n: u32| {
        let action = Action {
            header: ActionHeader {
                author: taker.author.clone(),
                timestamp: Timestamp(taker.ts),
                action_seq: taker.seq + 1,
                prev_action: Some(taker.head.clone()),
            },
            data: ActionData::CreateLink(CreateLinkData {
                base_address: base,
                target_address: reclaim.clone().into(),
                zome_index: ZomeIndex(0),
                link_type: LinkType(LinkTypes::EscrowToReclaims as u8),
                tag: LinkTag::new(vec![]),
            }),
        };
        Record::new(signed(action, ActionHash::from_raw_36(hash36(0xa2, n))), RecordEntry::NA)
    };
    let good = link(s.escrow.clone().into(), &s.taker, 1);
    assert!(is_valid(&s.w.verdict(&good)));
    let bad = link(s.park.clone().into(), &s.taker, 2);
    assert_invalid(&s.w.verdict(&bad), "park's escrow");
}
