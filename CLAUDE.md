# CLAUDE.md

Guidance for Claude Code working in this repo.

## Project

Holochain DEX MVP (UNIT-A / UNIT-B limit orders) with a mock Unyt ledger. See
README.md for design, invariants and known limitations. Use the `holochain`,
`holochain-dev` and `unyt-smart-agreement` skills.

## Commands (always inside the Nix shell)

- `nix develop -c cargo test -p dex_core` — settlement logic unit tests
- `nix develop -c ./build.sh` — build wasm, pack `dex.dna` and `dex.happ`
- `nix develop -c cargo test --manifest-path tests/sweettest/Cargo.toml` — conductor tests (needs `./build.sh` first)
- In `ui/`: `nix develop .. -c npm start` (two agents via hc-spin), `npm test` (Vitest),
  `npm run check` (svelte-check), `npm run e2e` (Playwright demo; starts `scripts/sandbox.sh`)
- UI types in `ui/src/lib/api.ts` mirror `ledger_api` / `dex_api` / `dex_core` by hand:
  change them together with the Rust structs.

## Rules

- Pinned: `hdk =0.7.0`, `hdi =0.8.0`, `holochain =0.7.0` (holonix `main-0.7`). Verify HDK/HDI APIs
  against the pinned crate source or docs.rs at that version, never from memory.
- `dnas/dex/zomes/integrity/**` and `dnas/dex/dna.yaml` change the DNA hash.
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

1. Run `./build.sh` and the Sweettest suite; fix anything the conductor surfaces
   (the zomes have only been type-checked natively so far).
2. `dex` integrity + coordinator zomes: `Order` discovery entries/links keyed by
   market side and price, price-level aggregation, calls into `ledger`.
3. Signals (post_commit) for park/run/collect notifications; maker auto-run.
4. UI.
5. Rhai Smart Agreement template ported from `dex_core::execute_run`, tested
   against the published `rave_engine` crate.
