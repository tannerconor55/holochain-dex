# Issue draft for the Holochain team: zome-call latency grows with source-chain / DHT size

Ready to file. Holochain 0.7.0 (hdk 0.7.0, hdi 0.8.0), stock Sweettest
conductors (`SweetConductorConfig::standard()`), two conductors on one
machine, both full-arc. Reproducer: `tests/sweettest/src/writes.rs` in this
repository (`DEX_WRITE_BENCH_MAX=1000 cargo test --manifest-path
tests/sweettest/Cargo.toml writes:: -- --ignored --nocapture`).

## Summary

The latency of a zome call that does no DHT or chain work grows roughly
linearly with the size of the calling agent's source chain (and the DHT it
holds): 16 ms at ~600 actions, 125 ms at ~6,000 (about 20 µs per action).
Every write grows the same way. App validation is bounded and flat
(measured natively), so the growth is not in app code.

## Numbers

One agent places and settles orders one at a time (each order: a few ledger
entries and links, ~5 actions). Means over 12 samples at each size:

| Orders of history (≈ actions on the caller's chain) | No-op call | Control write (`mint`: 1 entry, O(1) validation) | Settle (≈ 5 actions, nested `call`s) |
|---:|---:|---:|---:|
| 100 (≈ 600) | 16 ms | 74 ms | 246 ms |
| 250 (≈ 1,500) | 37 ms | 106 ms | 392 ms |
| 500 (≈ 3,000) | 67 ms | 181 ms | 631 ms |
| 1000 (≈ 6,000) | 125 ms | 353 ms | 1,317 ms |

The no-op call is `get_config`: it calls `dna_info()` and returns a value
derived from the DNA properties; no `get`, `get_links`, `query` or write.

App validation, timed natively (the real `validate` callback on hand-built
chains of 100–1000 entries, with a fake `HdiT`): an escrow (debit) 12–37 µs,
a checkpoint 13–38 µs, a run 13 µs, flat in chain length.

## Also observed (may be by design; documenting in case it is not)

1. **Inline validation runs at the end of every zome call, nested calls
   included, over all of the outer call's scratch records**
   (`holochain` 0.7.0 `core/workflow/call_zome_workflow.rs`:
   `inline_validation` is called from `call_zome_workflow_inner`; only the
   flush is gated on `is_root_zome_call`). An outer call that writes and then
   makes N more local `call`s validates its earlier writes N + 1 times. We
   restructured our coordinator to make every read before any write; is
   validating only the nested call's own new records (or only at the root)
   possible?
2. **Every `Create` is app-validated once per op type** (`CreateRecord`,
   `CreateEntry`, and `AgentActivity`) during inline validation, so an app
   that does real work for both record and entry ops pays it twice per pass.
3. **`must_get_agent_activity(..., include_cached_entries())` on the
   store path returns `cached_entry: None`** (`holochain_state` 0.7.0
   `dht_store/reads.rs` `must_get_agent_activity`), so a bounded segment walk
   then fetches each entry with its own `must_get_entry`.

## Questions

- What in a zome call scales with chain or DHT size for a call that touches
  neither? (Candidates we could not confirm from the source: workspace setup,
  background workflows competing for the same runtime as the DHT grows,
  database growth without the needed indexes.)
- Is (1) intended, and is there a supported way for a coordinator to batch
  writes across zomes without re-validating them per nested call?
