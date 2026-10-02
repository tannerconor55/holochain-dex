# Run-write performance (follow-up to milestone 4)

Status: coordinator fixes done; options below are for approval. No
validation change has been made.

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
