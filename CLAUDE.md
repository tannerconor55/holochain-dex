# CLAUDE.md

Guidance for Claude Code working in this repo.

## Project

Holochain DEX MVP (limit and market orders; markets declared in DNA
properties, all quoted in the hub unit HF; demo market A/HF) with a mock
Unyt ledger. See README.md for design, invariants and known limitations, and
`docs/design/taker-protection.md` for park deadlines, reclaim and
checkpoints. Use the `holochain`, `holochain-dev` and `unyt-smart-agreement`
skills.

## Commands (always inside the Nix shell)

- `nix develop -c cargo test -p dex_core` — settlement logic unit tests
- `nix develop -c cargo test -p ledger_integrity` — crafted-chain attack traces against the real `validate`
- `nix develop -c ./build.sh` — build wasm, pack `dex.dna` and `dex.happ`
- `nix develop -c cargo test --manifest-path tests/sweettest/Cargo.toml` — conductor tests (needs `./build.sh` first; ~4 min, first build ~12 min)
- `nix develop -c cargo test --manifest-path tests/rhai_parity/Cargo.toml` — `unyt/dex_order_escrow` vs `dex_core` under rave_engine; rerun after any change to `execute_run` or the template
- In `ui/`: `nix develop .. -c npm start` (two agents via hc-spin), `npm test` (Vitest),
  `npm run check` (svelte-check), `npm run e2e` (Playwright demo; starts `scripts/sandbox.sh`)
- UI types in `ui/src/lib/api.ts` mirror `ledger_api` / `dex_api` / `dex_core` by hand:
  change them together with the Rust structs.

## Rules

- Pinned: `hdk =0.7.0`, `hdi =0.8.0`, `holochain =0.7.0` (holonix `main-0.7`). Verify HDK/HDI APIs
  against the pinned crate source or docs.rs at that version, never from memory.
- `dnas/dex/zomes/integrity/**` (except `integrity/*/tests/`) and
  `dnas/dex/dna.yaml`, including its properties, change the DNA hash.
  Flag it in every change; commit integrity changes separately.
- Settlement arithmetic lives in `crates/dex_core` only, as integer minor units.
  Validation and coordinator both call `dex_core::execute_run`; never fork the
  logic. It is the reference for the future Rhai template.
- The `ledger` coordinator is the settlement interface. DEX/order-book code must
  not touch ledger entries directly, so the mock can be swapped for Unyt.
- Tests: Sweettest only (no Tryorama). Use `await_consistency` before any
  cross-agent read.
- No `unwrap()`/`expect()` in zome code.

## Next steps

Done (see PLAN.md status): order book zomes, signals and maker auto-run
(inline, synchronous commits), UI, market orders, the Unyt template
`unyt/dex_order_escrow`, proven against `dex_core::execute_run` by
`tests/rhai_parity` (keep the two in step), and milestone 2: park deadlines
and anchored reclaim, balance checkpoints, multi-market with the HF hub.

Next: answer the Unyt questions in README "Swapping in real Unyt", then
replace the mock `ledger` zomes with Unyt agreements built from the template.
