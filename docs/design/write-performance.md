# Run-write performance (follow-up to milestone 4)

Status: measured (option 1 done); the conductor dominates; coordinator-only
reductions done; **MVP acceptance met** (settle 1,317 ms, cancel 1,130 ms at
1000 orders of history, target ~1.5 s). No validation change. Issue for the
Holochain team: `holochain-issue-call-overhead.md`.

## Measured (realistic case)

`tests/sweettest/src/writes.rs` (`#[ignore]`): one maker; orders are placed
and settled as they arrive (every fourth cancelled), so each run's escrow is
recent. Times are means over 12 orders at each history size, ms:

| Orders of history | Place | Ledger coordinator (`plan_run`) | Ledger run (`run_escrow`) | Settle (dex) | Cancel (dex) |
|---:|---:|---:|---:|---:|---:|
| 100 (before) | 212 | 112 | 206 | 366 | 391 |
| 100 (after) | 216 | 27 | 124 | 286 | 320 |
| 250 (before) | 316 | 265 | 416 | 595 | 545 |
| 250 (after) | 333 | 54 | 218 | 432 | 384 |
| 500 (before) | 472 | 505 | 738 | 1,009 | 978 |
| 500 (after) | 499 | 96 | 303 | 637 | 638 |
| 1000 (before) | 886 | 983 | 1,430 | 1,862 | 1,889 |
| 1000 (after) | 840 | 180 | 617 | 1,371 | 1,189 |

"After" is commit `99da9e3`: `run_escrow` reads only the runs since the
escrow (range- and type-filtered query) instead of decoding every run the
maker has, and unlisting finds the maker's own listing link on their own
chain instead of scanning the day's listing bucket.

How to read the columns: `plan_run` is everything `run_escrow` does before
writing (the ledger coordinator); `run_escrow − plan_run` is the commit and
its inline validation; `settle − run_escrow` is the dex coordinator's own
reads plus the extra commits it makes (the trade index link, the listing
delete) and their validation.

At 1000 orders of history (after):

| Part | Cost | Trend |
|---|---:|---|
| Ledger coordinator | ~180 ms | was 983; still grows (27 → 180) |
| Commit + validation of the run | ~437 ms | unchanged (was 447); grows ~4.5× from 100 to 1000 |
| Dex side (reads, index link, unlisting, their commits) | ~754 ms | grew (was 432) |
| Place (escrow + 2 links + checkpoint check) | ~840 ms | grows ~4× although its app validation is bounded |

**Target (settle and cancel under ~1 s at 1000) is missed**: 1,371 ms and
1,189 ms. The coordinator was the main cost and is no longer; commit and
validation now are.

## What is and is not known

- App validation is bounded by design: debits, collects and reclaims walk
  at most the segment since a checkpoint (≤ 256 actions); a run walks back
  only to its previous run or escrow (a few actions here); index and
  listing-delete validation is a few `must_get`s. Holochain's
  `must_get_agent_activity` with `until_hash` scans only that sequence range
  (`holochain_state` 0.7.0 `dht_store/reads.rs:1578`).
- Yet placing an order, whose app validation does not depend on history,
  grows from 216 to 840 ms. That points to per-commit cost in the conductor
  (database size, system validation, publishing and integration, the
  validation of the other agent's ops arriving by gossip), not our rules.
  **Not proven**: the benchmark cannot separate the two.
- Found while reading: on the path validation uses, Holochain returns no
  cached entries for `must_get_agent_activity`, so each segment walk fetches
  each ledger entry with its own `must_get_entry` (bounded, ≤ 256).
- The ledger coordinator still grows (27 → 180 ms). Candidates: the
  source-chain `query` filters may be applied after loading more rows than
  the range, and `prepare_run`'s network reads (escrow, parks) slow as the
  DHT grows. Not yet attributed.

## Options (for approval)

1. **Measure before changing anything (recommended next step).**
   - Add a control write to the benchmark: a `mint` (O(1) app validation)
     timed at each history size. If it grows like `place`, the growth is
     the conductor's per-commit cost, not our validation.
   - Time `ledger_integrity::validate` natively on crafted chains of 100 to
     1000 entries (the crafted-chain harness), for a run, an escrow and a
     checkpoint: our validation cost with no conductor around it.
   - Profile one settle with `RUST_LOG` timing / tracing spans to see where
     the conductor spends the remaining time.
   Coordinator/test-only; no DNA change.
2. **Coordinator-only reductions, if (1) points at commit count.** A settle
   commits the run, an `EscrowToRuns` link, one `AgentToIncomingRuns` link
   per receiver, the trade index link, the listing delete, and sometimes a
   checkpoint: 5–7 actions, each validated. Checkpointing more often than
   `CHECKPOINT_EVERY` (the coordinator decides when; the constants are only
   upper bounds) would shorten every debit's validation walk. No DNA change.
3. **Validation changes, only if (1) shows our validation dominates**
   (DNA-hash impacting; would need your approval):
   - checkpoint segments: cite and fetch the segment's entries once (or
     carry the entries' hashes in the checkpoint) instead of a
     `must_get_entry` per entry;
   - a lower `MAX_ACTIONS_SINCE_CHECKPOINT` so walks are shorter.
4. **If the conductor's per-commit cost dominates:** nothing in this repo
   removes it; fewer actions per settle (option 2) is the lever, and the
   numbers become a question for Holochain (per-commit cost vs DB size on
   0.7.0).

## Recommendation

Do (1) first: two benchmark additions and a native validation timing, a
few hours of runtime, no zome change. Then decide between (2), (3) and (4)
on data rather than guesses.

## Option 1 results (measure first)

**Native `validate()`** (crafted chains, release build; ignored test
`validation_cost_by_history` in `ledger_integrity/tests/crafted_chains.rs`):

| Entries of history | Escrow (debit) | Checkpoint | Run (fresh order) |
|---:|---:|---:|---:|
| 100 | 12 µs | 13 µs | 13 µs |
| 250 | 37 µs | 38 µs | 14 µs |
| 500 | 29 µs | 29 µs | 13 µs |
| 1000 | 17 µs | 18 µs | 13 µs |

Flat in history (escrow and checkpoint vary with the segment since the last
checkpoint, 0–32 entries). Our validation logic is bounded and microseconds.

**Holochain 0.7 source: nested `call()`s share the workspace but each runs
inline validation.** A local `call` passes the caller's workspace to the
callee (`host_fn/call.rs`: `call_zome_with_workspace`), so ledger writes
made from the dex zome commit together with the dex zome's own writes, in
one flush at the end of the outer call (`call_zome_workflow.rs`: the flush
is gated on `is_root_zome_call`). But `inline_validation` runs at the end of
**every** call, nested or not, over **all** scratch records so far; and each
`Create` is validated once per op type (`CreateRecord`, `CreateEntry`,
`AgentActivity`). A settle used to validate its run in three passes (end of
`run_escrow`, end of the `get_escrow_state` read after it, the outer call),
twice per pass.

**Control write and no-op call, and the traced settle**, after restructuring
(`1967fb4`: every ledger read before the run writes):

| Orders of history | No-op call | Control mint | Place | Settle | Cancel |
|---:|---:|---:|---:|---:|---:|
| 100 | 16 ms | 74 ms | 219 ms | 246 ms | 234 ms |
| 250 | 37 ms | 106 ms | 308 ms | 392 ms | 412 ms |
| 500 | 67 ms | 181 ms | 492 ms | 631 ms | 669 ms |
| 1000 | 125 ms | 353 ms | 849 ms | 1,317 ms | 1,130 ms |

Inside one settle at 1000 (traced): the nested `run_escrow` with its inline
validation 545 ms, the two pre-run reads 83 + 72 ms, the index link 89 ms,
unlisting 131 ms; the outer call's own validation and flush the remaining
~380 ms of the 1,303 ms.

**Verdict: the conductor dominates.** A call that does no DHT or chain work
grows 8× from 100 to 1000 orders of history (16 → 125 ms), the control
mint ~5×, and every step of a settle in proportion, while our validation is
flat. Per-call overhead in the conductor grows with the chain / DHT; we have
not found its cause in the 0.7.0 source (the chain-head lookup is a
`max(seq)` query; an author's own call skips the cap-grant lookup).

## Option 2 done (coordinator-only)

- `1967fb4`: no ledger call after a run writes (removes a validation pass
  of the run and its links); `run_my_orders` reads every order before
  running any.
- `e043531`: the order's state and pending parks in one ledger call
  (`get_order_context`) instead of two. Measured to 500 orders of history:
  the read 47 ms instead of 47 + 39; settle 631 → 609 ms, cancel 669 →
  635 ms (100: 246 → 238, 234 → 199; 250: 392 → 372, 412 → 395).

Not done, with reasons: a settle's actions (run, `EscrowToRuns`, one
`AgentToIncomingRuns` per receiver, the trade index link, the listing
delete) are each needed; dropping the maker's own incoming-run link would
bring back an O(runs) read in `collect_all`. Placing an order needs its
nested ledger write by design (the ledger owns escrows).

## What remains

- Per-call conductor overhead growing with history: reported to Holochain
  (`holochain-issue-call-overhead.md`).
- Validation options (DNA-hash impacting, not needed for the MVP target):
  do the heavy checks only for the `CreateRecord` op (not again for
  `CreateEntry`) to halve inline validation per pass; carry segment entry
  hashes in checkpoints to avoid a `must_get_entry` per entry.
- Known race: right after an order appears, a take can fail with
  "DepMissingFromDht ... may be retried" until the escrow reaches the
  taker's node; retrying works (seen once in Playwright).
