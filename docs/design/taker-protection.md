# Taker protection and validation performance

Milestone 2 design, for approval. One integrity (DNA-hash) change covers:

- **A.** Park timeout: a taker can reclaim a park the maker never settled.
- **B.** Balance checkpoints: debit validation stops walking the whole chain.

Two client-side items with no integrity change are sketched at the end:
**C.** a maker presence check before parking, and **D.** faster Sweettests.

Multi-market support (generic pair, per-market lot and tick size) is being
folded into the same integrity change (PLAN.md, decided 2026-09-30). It is
not designed here.

Version note: the prompt names hdk 0.6.3 / hdi 0.7.3 / holochain 0.6.3. The
repo has pinned hdk 0.7.0 / hdi 0.8.0 / holochain 0.7.0 since the upgrade.
Every Holochain claim below was read in the 0.7.0 source and checked against
0.6.3, and they agree.

---

## 1. What Holochain enforces about timestamps

From the crate source, not memory:

| Rule | Where | Enforced? |
|---|---|---|
| An action's timestamp is `>=` its `prev_action`'s | `holochain` 0.7.0 `core/sys_validate.rs::check_prev_timestamp` (`if t2 >= t1`), called from `sys_validation_workflow.rs::store_record`; identical in 0.6.3 (`sys_validate.rs:259`, called at `sys_validation_workflow.rs:1271`) | **Yes.** Non-decreasing, so equal timestamps are allowed. |
| An action is not too far in the future | none | **No.** `ValidationOutcome` (`sys_validate/error.rs`) has no time variant. The only time error is `PrevActionErrorKind::Timestamp` (`holochain_types` `chain/chain_item.rs`), the rule above. |
| An action is not backdated | none, beyond the rule above | **No.** An author may stamp any time `>=` their own previous action. |
| `DnaModifiers.origin_time` as a floor | not referenced by system validation | **No.** |
| Honest authoring stamps `max(now, chain_head + 1µs)` | `holochain_state` `source_chain.rs` | Author-side behaviour only. A modified conductor can write anything the first rule allows. |
| Chain forks (two actions with one `prev_action`) | `sys_validation_workflow.rs` issues `ChainIntegrityWarrant::ChainFork` | **Detected and warranted, not prevented.** Ops already validated on each branch stand. |
| Countersigning (atomic multi-chain actions) | `holochain` feature `unstable-countersigning` | **Not in the default features** (`encryption`, `schema`, `wasmer-sys-cranelift`), so a stock holonix conductor can't countersign. |

Consequences we design against:

- A maker can backdate a run to **any time at or after their own last
  action**. If their last action was at `t0`, a run written years later may
  still claim `t0 + 1µs`.
- Nothing in validation knows "now". Deterministic validation (no `get`, no
  `sys_time`) can only compare timestamps that the chains themselves carry.
- `must_get_agent_activity(author, ChainFilter::until_hash(top, bottom))`
  follows `prev_action` from `top` [hdi 0.8.0 `chain.rs`], so it reads exactly
  one branch. A fork is only visible to fork detection.

## 2. Safety property and threat model

**Property P.** For any park, at most one of these is ever valid: (1) a
settlement run that consumes it, (2) a Reclaim of it. This must hold under any
interleaving, any offline period, and any timestamps the rules above allow.

**Liveness goal L.** A taker whose park is never settled can get their funds
back without the maker doing anything.

Adversaries:

- **Maker:** may backdate within the rules, stay offline, go offline forever,
  choose which parks a run consumes, and fork (warranted).
- **Taker:** may backdate their own actions, cite any hash, reclaim early or
  twice, and fork (warranted).
- **Maker and taker colluding:** considered where it changes the answer.

Money at stake if P fails: the park's funds count twice. The run pays them to
the maker, and the Reclaim credits them back to the taker, which inflates
supply.

## 3. The deadline

Each park gets a deadline derived from data validation can read. Nothing new
is stored:

```
deadline(park) = min(park.timestamp + PARK_TIMEOUT, escrow.expires_at) + SETTLE_GRACE
```

- `park.timestamp` is the park action's header timestamp (taker-asserted).
  `escrow.expires_at` is from the order terms (maker-asserted).
- Proposed constants, integrity-level and so DNA-hash impacting:
  `PARK_TIMEOUT = 30 min`, `SETTLE_GRACE = 5 min`. An online maker's client
  settles within seconds (signal plus 10 s poll), so 30 minutes is generous;
  the `min` with expiry means that after an order expires, pending parks can
  only be refunded by the maker within the grace, and then by Reclaim.
- **A backdating taker only hurts themselves.** An earlier `park.timestamp`
  gives an earlier deadline, so the park becomes unconsumable (and
  reclaimable) sooner. The maker's coordinator must skip parks whose deadline
  is too close. It does, with a 60 s safety margin; otherwise the maker's own
  run would fail validation.

## 4. Candidates

### 4.1 Deadline rule alone

*Rule:* a run may consume a park only if `run.timestamp < deadline`. A
Reclaim is valid only if `reclaim.timestamp >= deadline`.

**Attack trace (maker backdates):**
1. `t0`: the maker opens the escrow. This is the maker's last action.
2. `t1 > t0`: the taker parks. `D = deadline(park) > t1`.
3. The maker stays offline.
4. `t2 > D`: the taker reclaims. It's valid because `t2 >= D`, and the taker
   has their funds back.
5. Years later the maker writes a run stamped `t0 + 1µs < D` that consumes
   the park. System validation accepts it (`>=` the maker's previous action),
   and app validation accepts it (`< D`).
6. **Both are valid, so P fails.** The park's funds are counted twice.

Nothing in the taker's chain can constrain a timestamp on the maker's chain.
**Verdict: unsafe. Rejected.**

### 4.2 Chain-anchored reclaim (recommended)

*Rules:*
- **Run:** may consume a park only if `run.timestamp < deadline(park)`.
- **Reclaim** cites an **anchor**: any action `A` on the **maker's** chain with
  `A.timestamp >= deadline(park)`. It is valid only if
  `must_get_agent_activity(maker, until_hash(A, escrow))`, the maker's
  chain from `A` back to the escrow's creation, contains no `SettlementRun`
  whose `consumed` includes the park. It must also be authored by the park's
  author, and must not duplicate an earlier Reclaim of the same park.

*Why P holds.* Timestamps are non-decreasing along the maker's chain
(section 1). Any valid consuming run has `ts < D <= A.ts`, so it cannot come
after `A`, where every action has `ts >= A.ts`. It must therefore come before
`A`, between the escrow and `A`. That is exactly the segment the Reclaim walks,
and the walk would have found it. Conversely, once a Reclaim cites `A`, no run
at or after `A` is valid, and no run can be inserted before `A` without
forking. Validation is deterministic: the walk is hash-bounded and reads only
chain data.

**Traces:**

| # | Scenario | Outcome |
|---|---|---|
| T1 | Maker settles in time: run at `ts < D` | Every anchor `A` with `A.ts >= D` comes after the run, so a Reclaim's walk finds the run. **Reclaim invalid; run valid.** |
| T2 | Maker is back online after `D` but never settles (buggy or hostile client): any action `A`, `A.ts >= D` | The Reclaim citing `A` walks the segment and finds no run. **Reclaim valid.** A later run consuming the park has `ts >= A.ts >= D`. **Run invalid.** |
| T3 | Maker backdates a run after writing `A` | Its `prev_action` is at or after `A`, so `ts >= A.ts >= D`. **Invalid.** |
| T4 | Maker races: run stamped `D − 1µs`, anchor after it | The run precedes every valid anchor, so any Reclaim finds it. **Run only.** |
| T5 | Taker reclaims early (cites `A` with `A.ts < D`) | **Reclaim invalid.** |
| T6 | Taker cites an action by someone other than the maker as the anchor | Anchor author must equal the escrow's author. **Invalid.** |
| T7 | Taker reclaims twice | Walking the taker's own chain from the Reclaim back to the park finds the first Reclaim. **Invalid.** |
| T8 | A third party reclaims someone else's park | Author must equal the park's author. **Invalid.** |
| T9 | Taker backdates their park | `D` comes earlier, so the park is unconsumable sooner. It only affects the taker; the maker's coordinator skips it. **P holds.** |
| T10 | Clock skew, honest maker behind | Runs are stamped earlier, so more parks are consumable. Anchors come later. **P holds.** |
| T11 | Clock skew, honest maker ahead | Parks become unconsumable sooner and reclaimable sooner. **P holds.** |
| T12 | Maker forks: on a branch from before `A`, writes a run with `ts < D` | Both are valid on their own branches. **P fails, but only through a fork**, which is detected and warranted (section 1). With a colluding taker this double-counts the park. It's the same exposure the ledger already has today, since a fork could also run one escrow's lock twice. |
| T13 | **Maker offline forever** (no action after `D` ever) | No anchor exists, so **no Reclaim is possible. L fails.** |

**Verdict: P holds except through a warranted fork. L holds whenever the
maker writes any action after the deadline, but not if they never write
again.**

### 4.3 Other candidates

**a. Taker's chain as the arbiter.** Runs must be accepted on the taker's
chain; the Reclaim proves its own chain has no acceptance. This is linear
and provable without the maker. But an acceptance on the taker's chain that
the maker's run can rely on needs both chains to record one atomic action,
which is **countersigning**. That isn't available on a stock conductor
(section 1). It would also require the taker to be online for every fill,
which defeats asynchronous settlement. With the feature enabled it would
meet P and L; T12's fork caveat applies to the taker instead. **Rejected for
this network.**

**b. Maker-signed acceptance before the deadline.** The maker signs
"accepted, will settle" per park; a Reclaim is valid after `D` if no
acceptance exists. Proving there is none needs the same anchor as 4.2, so it
gains nothing and adds a round trip. **Rejected.**

**c. A pre-signed refund voucher at order opening.** It lets the taker
reclaim without the maker online. But it cannot stop a later backdated run
(the 4.1 trace applies unchanged). **Unsafe. Rejected.**

**d. A trusted timestamp or notary agent.** The Reclaim cites an attestation
by a notary: "the maker's chain at time T had no consuming run". This
changes the trust model to trusting the notary, and the notary has to watch
the maker's chain. **Possible, but out of scope**; noted for later.

**e. A maker heartbeat written to the chain.** Every heartbeat after `D` would
serve as an anchor, so any online maker becomes reclaimable-against. It
doesn't help the offline-forever case, and it costs a chain action and a DHT
publish every period. **Not needed:** any maker action is already an anchor
under 4.2.

## 5. Recommendation

**No candidate meets both P and L with the maker offline forever on a stock
Holochain 0.7 conductor.** Timestamps are author-asserted with no lower bound
beyond the author's own previous action (section 1). So proving that the maker
will never write a backdated consuming run needs either a later maker action
(4.2) or an atomic action on the taker's chain (countersigning, 4.3a). Neither
exists when the maker is gone.

**Adopt 4.2, the chain-anchored reclaim.** It is safe (P holds except through
a warranted fork, the ledger's existing exposure). It makes the taker
independent of the maker's *cooperation*: any action the maker writes after
the deadline (a trade, a collect, even a mint) unlocks every stuck park,
whether or not their client ever settles. Trapping takers' funds requires the
maker to abandon their whole account on this network, including their own
escrowed lock.

**README limitations wording (to replace "Maker liveness"):**

> **Maker liveness.** A park the maker has not settled within its deadline
> (30 minutes after parking, or the order's expiry if sooner, plus 5 minutes'
> grace) can be reclaimed by the taker, and the maker's cooperation is not
> needed: any action the maker writes on this network after the deadline
> (settling another order, collecting, trading) is enough to prove the park
> was never settled. If the maker never writes anything again, the park
> cannot be reclaimed: validation has no clock, and Holochain lets an author
> backdate an action to their own last action, so only a later maker action
> can prove no settlement is still to come. The maker's own escrowed funds
> stay locked in the same way.

## 6. Effect on the Rhai template and on Unyt

- **Settlement semantics change:** a run may no longer consume a park at or
  after its deadline. `dex_core` gets the rule, and the template must mirror
  it or parity breaks.
- **The template cannot compute the deadline from chain data [engine].** The
  helper that sorts by parked-link timestamp, `acceding_sort_allocation`, does
  not return the timestamp; it returns `{amount, source, spender}`. No other
  registered helper exposes it (`rhai_helper_functions` catalog;
  `dna_helper.rs`). Options: (i) the taker declares `park_deadline` in the
  spend payload beside `requested_lots`, a taker claim that is harmless for
  the same reason T9 is; (ii) ask Unyt for a helper that returns the parked
  link timestamp. I propose (i) for parity, with (ii) added to the Unyt
  questions. With (i), a park past its deadline goes to `rejected_links`, so
  it stays on chain for reclaim [engine: rejected links are not consumed],
  instead of being consumed.
- **Reclaim on Unyt is [DNA].** Whether a parked spend can be withdrawn by
  its author is README Unyt question 1, still open. `rave_engine`'s `Reclaim`
  entry is for rejected commitments (`Reclaim { rejection, … }`,
  `types/entries/reclaim.rs`), not parked spends [engine]. The anchored-reclaim
  rule would have to be Unyt DNA logic; a template cannot write it.
- **Checkpoints do not affect the template.** They are internal to the mock
  ledger.

## 7. B. Balance checkpoints

**Today.** `validate_debit` (Escrow, Park) and `validate_collect` call
`walk_chain`, which runs `must_get_agent_activity` from the action's
`prev_action` to genesis. That is **O(n) per debit for an n-action chain, and
O(n²) to validate a whole chain.** `validate_run` walks the maker's whole
chain as well, to check `prev_run` and past consumption.

**Proposal.**

```rust
struct Checkpoint {
    prev: Option<ActionHash>,          // the author's previous Checkpoint
    totals: LedgerTotals,              // cumulative since genesis
    collected: Vec<(ActionHash, u32)>, // every (run, allocation index) ever collected, sorted
}
struct LedgerTotals { minted: Amounts, collected: Amounts, reclaimed: Amounts, escrowed: Amounts, parked: Amounts }
// available = minted + collected + reclaimed − escrowed − parked
```

- **Checkpoint validation.** `prev` must be the author's latest checkpoint.
  Walk `until_hash(checkpoint.prev_action, prev)`, reject the checkpoint if
  the segment holds another `Checkpoint`, and require
  `totals == prev.totals + segment entries` and
  `collected == prev.collected ∪ segment collects`, sorted and unique. The
  first checkpoint walks to genesis.
- **Debits, collects and reclaims cite** `checkpoint: Option<ActionHash>`,
  the author's latest. Validation walks only
  `until_hash(prev_action, checkpoint)` and **rejects a stale citation** if the
  segment contains a newer `Checkpoint`. Balance = `checkpoint.totals` +
  segment. A collect is a duplicate if its `(run, index)` is in
  `checkpoint.collected` (binary search) or in a segment Collect.
- **Bounding validator cost** (so skipping checkpoints isn't a
  denial-of-service lever): a ledger entry is invalid if its segment since
  the cited checkpoint exceeds `MAX_ACTIONS_SINCE_CHECKPOINT = 256` actions.
- **Coordinator cadence:** after any ledger write, write a checkpoint in the
  same call once 32 ledger entries have been written since the last one.
  With about 3 actions per ledger entry (entry plus links), that's roughly
  100 actions, comfortably under the 256 hard limit even with dex zome links
  interleaved. The cost is one checkpoint per 32 entries, about 3% chain
  growth.
- **Runs stop walking the whole chain as well.** `prev_run` must be the
  latest run for this escrow: walk `until_hash(run.prev_action, prev_run)`, or
  back to the escrow for the first run, and reject if another run for this
  escrow appears in the segment. Past consumption: follow the `prev_run`
  chain (`must_get_valid_record` per run) and union their `consumed` lists.
  That costs O(runs × parks for this escrow), bounded by the order's life,
  not the chain.
- **Complexity after the change:** a debit or collect costs O(256) segment
  plus one checkpoint fetch, plus O(|collected|) to decode the set for
  collects and checkpoints. Validating a whole chain is O(n), not O(n²).
  The collected set grows about 40 bytes per collect, so 100,000 collects fit
  well under the 4 MB entry limit. A Merkle accumulator would make it
  O(log n), noted as future work.
- **Migration: none needed.** This is a new DNA hash and a new network, and
  only test networks exist. No old entry needs to decode, so there's no
  `#[serde(default)]`.

## 8. Exact changes (integrity; DNA-hash impacting)

`ledger_integrity`:

1. **New entry types.** `Reclaim { park: ActionHash, anchor: ActionHash,
   checkpoint: Option<ActionHash> }` and `Checkpoint { … }` (section 7).
2. **New link.** `EscrowToReclaims` (escrow → Reclaim), so makers and readers
   see reclaimed parks. It's valid only if the target is a Reclaim by the link
   author whose park targets the base escrow.
3. **New field.** `checkpoint: Option<ActionHash>` on `Escrow`, `Park` and
   `Collect`.
4. **SettlementRun.** Every consumed park must satisfy
   `run.timestamp < deadline(park)`, and must not have been reclaimed.
   Reclaims come after anchors, and anchors have `ts >= D`, so this is implied
   by P and needs no extra check. `prev_run` and past-consumption checks use
   the bounded walks in section 7.
5. **Reclaim** validation, per section 4.2: park author equals reclaim author;
   anchor author equals escrow author; `anchor.ts >= deadline`; the maker walk
   from anchor to escrow finds no consuming run; the own-chain walk from the
   Reclaim back to the park finds no earlier Reclaim of it; the checkpoint
   citation rules apply.
6. **Balance.** Credits = mints + collects + reclaims. Debits = escrow locks +
   parks. Computed from the checkpoint plus the segment.
7. **Checkpoint** validation, and `MAX_ACTIONS_SINCE_CHECKPOINT`.
8. **Constants** in `dex_core` (compiled into integrity): `PARK_TIMEOUT`,
   `SETTLE_GRACE`, `MAX_ACTIONS_SINCE_CHECKPOINT`.

`dex_core` (pure): `park_deadline`, `consumable(run_ts, deadline)`,
`reclaimable(anchor_ts, deadline)`, `LedgerTotals` arithmetic, and
`Checkpoint::extend(segment)`.

Coordinators, no integrity change:
- **Ledger.** `reclaim_park` finds an anchor by reading the maker's agent
  activity: the chain head, and its timestamp via `get`. It then writes a
  Reclaim plus an `EscrowToReclaims` link. It also adds auto-checkpointing,
  and `ParkStatus` gains `deadline` and `reclaimable`.
- **Run.** `run_escrow` and pending-park reads skip reclaimed parks and parks
  with `deadline <= now + 60 s`.

## 9. How the tests will prove P

Honest conductors stamp `max(now, head + 1µs)`, so a Sweettest cannot author a
backdated action. The proof therefore has three layers:

1. **`dex_core` property test.** Generated maker chains (non-decreasing
   timestamps) with a park deadline, runs at arbitrary positions and
   timestamps, and anchors at arbitrary positions. Using the pure rules, it
   asserts that no generated history has both a valid consuming run and a
   valid Reclaim, and that every history with an action after `D` and no run
   allows a Reclaim.
2. **Integrity-level tests with crafted actions.** These call
   `ledger_integrity::validate` natively with `MockHdkT` answering
   `must_get_agent_activity` and `must_get_valid_record` with hand-built chains
   and **arbitrary timestamps**. That is the only way to put a backdated run in
   front of the real validation code. Each trace T1–T11 becomes a test that
   ends with exactly one of run or Reclaim valid, including the backdated run
   after the anchor (T3) and the race at `D − 1µs` (T4). T12 (fork) is shown
   as a documented pair of branch-valid ops; fork detection itself is
   Holochain's and isn't retested.
3. **Sweettests on real conductors.** The maker opens an order; the taker
   parks; the maker stays idle past the deadline; a Reclaim with no anchor is
   rejected; the maker then writes any action; the Reclaim succeeds and
   balances are restored. Also: an early Reclaim is rejected; a double Reclaim
   is rejected; a Reclaim of a settled park is rejected; a maker's run after
   the Reclaim can't consume the park; and on a checkpointed chain of 200+
   entries the balances are correct, a forged checkpoint total is rejected
   (crafted through a test extern that writes an arbitrary checkpoint), and a
   stale checkpoint citation is rejected. Total supply equals minted after
   every scenario.

Short deadlines make these tests fast: a debug-only constant override isn't
possible in integrity code without changing the DNA hash per build. Instead,
parks on orders with a 1-minute expiry have
`deadline = expiry + SETTLE_GRACE`, so the Sweettests will wait about
6 minutes of wall-clock time. I'll run those scenarios in parallel, and flag
this if it's too slow.

## 10. C and D (no integrity change)

**C. Presence check.** Options, by cost:

- **Remote-signal ping and reply.** No chain writes, but it needs a
  request/response protocol over fire-and-forget signals.
- **`call_remote` to a `ping` extern on the maker's cell.** No chain writes,
  one network round trip, and it fails fast with a `NetworkError` or timeout
  when the maker is offline. It needs an unrestricted cap grant for `ping`
  only.
- **A periodic heartbeat link or entry.** Asynchronous, but it adds a chain
  action and a DHT publish every period, and it grows the checkpoint segment.

**Choose `call_remote` ping.** The taker's UI pings the maker(s) of the
planned fills before `take` or a market order, warns and blocks if any maker
is unreachable (with an override), and shows a reclaim countdown plus a
Reclaim button on stuck parks.

**D. Faster Sweettests.** Conductor startup dominates. Plan: one shared
conductor batch per test binary, with a fresh app and fresh agents per
scenario on the shared conductors. Conductors can't cross `#[tokio::test]`
runtimes, so the scenarios will run on one shared runtime (or be grouped). I'll
report suite time before (about 23 min for 16 tests) and after.
