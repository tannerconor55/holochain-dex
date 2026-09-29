# DEX MVP — Holochain + (mock) Unyt

A peer-to-peer limit-order exchange for two test assets, **UNIT-A / UNIT-B**,
built on Holochain. Settlement follows the design planned for a Unyt Smart
Agreement, but runs against a **mock Unyt ledger** inside this DNA until Unyt
API access is available.

Spec documents: *DEX MVP Protocol v0.1* and *Smart Agreement Plan v0.1*.

## Status

| Piece | State |
|---|---|
| `dex_core`: settlement logic, order book views, listing tags, order status | Done. 36 unit tests. |
| `ledger_integrity` / `ledger` (mock Unyt ledger) | Done. Runs on holochain 0.7.0. |
| `dex_integrity`: listing links, validated against the escrow | Done. Wrong tag and wrong author rejected in a conductor. |
| `dex` coordinator: listing, book, take planning, maker settlement | Done. |
| Signals (`park_placed`, `run_settled`) and maker auto-run | Done; the UI drives the auto-run. |
| Sweettest suite | 12 tests pass (~15 min; each test starts its own conductors). |
| UI (`ui/`): wallet, book, ticket, take flow, my orders, activity | Done. 13 Vitest tests; the demo script passes in Playwright against two real conductors. |
| Rhai Smart Agreement template for real Unyt | Later; port of `dex_core::execute_run`. |

## Layout

```
Cargo.toml                          workspace (zomes + dex_core)
crates/dex_core/                    pure settlement logic, no Holochain deps
dnas/dex/dna.yaml                   DNA manifest
dnas/dex/zomes/integrity/ledger/    mock Unyt ledger: entry types, validation
dnas/dex/zomes/coordinator/ledger/  mock Unyt ledger: zome functions
workdir/happ.yaml                   hApp manifest (role "dex")
tests/sweettest/                    multi-agent conductor tests (own workspace)
flake.nix                           holonix main-0.7 dev shell
build.sh                            wasm build + dna/happ pack
```

## Build and test

```bash
nix develop                       # holonix 0.7 shell: rust, wasm target, hc, holochain
cargo test -p dex_core            # fast: settlement logic
./build.sh                        # zomes -> wasm -> dex.dna -> dex.happ
cargo test --manifest-path tests/sweettest/Cargo.toml   # conductor tests
```

Keep `tests/sweettest/Cargo.lock`: it pins a resolution of the conductor's
dependency graph known to build and pass. On 0.6.3 a fresh resolve picked
`ed25519` 3.0.0 final and broke `ed25519-dalek` inside the iroh transport;
treat a lockfile regeneration as a change to test.

Versions: `hdk = "=0.7.0"`, `hdi = "=0.8.0"`, `holochain = "=0.7.0"`
(holonix `main-0.7`). Upgraded from 0.6.3 before the order book zomes were
built on top. 0.6 and 0.7 conductors form separate networks with no data
migration. When the mock ledger is replaced, the Holochain version must match
what Unyt runs.

## Run the UI (two agents)

The UI lives in `ui/` (Svelte 5, Vite, `@holochain/client` 0.21). Every
command runs inside the dev shell, which provides `holochain`, `lair-keystore`
and the local bootstrap/relay server. Build the hApp first.

```bash
nix develop -c ./build.sh
cd ui && nix develop .. -c npm install

# Two agents in two windows (hc-spin starts the conductors and a local
# bootstrap + relay server):
nix develop .. -c npm start

# Or two browser tabs against two sandboxes: start them, then `npm run dev`
# and open the two URLs the script prints (?admin_port=…&app_port=…).
nix develop -c ./scripts/sandbox.sh

nix develop .. -c npm test          # Vitest: amounts, API layer, signal parsing
nix develop .. -c npm run check     # svelte-check
nix develop .. -c npm run e2e       # Playwright: the demo script in two tabs
```

A maker's orders settle automatically only while their app is open: the UI
runs `run_my_orders` on every `park_placed` signal and every 10 s poll, and
collects allocations on `run_settled` and on the poll.

## How settlement works

Each order is an **escrow** (the stand-in for one Unyt Smart Agreement
instance). Its action hash is the order's identity.

1. **Open.** The maker creates an `Escrow` with the order terms. Validation
   debits the lock (e.g. 100.00 A for a 100-lot sell) from their balance.
2. **Park.** A taker creates a `Park`: funds plus the lots they want. Their
   balance is debited.
3. **Run.** The maker (and only the maker) creates a `SettlementRun`. It
   consumes up to 20 pending parks in time order and, for each, pays the
   taker the maker asset, pays the maker the quote, and refunds any unused
   funds. The remainder stays locked. `Release` mode (cancel or post-expiry)
   refunds consumed parks and returns the whole lock to the maker.
4. **Collect.** Each receiver creates a `Collect` for their allocation, which
   credits their balance.

Every validator re-executes the run with `dex_core::execute_run` and requires
an exact match, like peers re-running a Unyt RAVE.

### Invariants enforced by validation

* **No negative balances**: `Escrow` and `Park` debits are checked against the
  author's balance, recomputed from their source chain.
* **No double-spend of an escrow**: runs are maker-only, and each must name the
  maker's latest run for that escrow as `prev_run`, checked by walking the
  maker's chain. A park can be consumed by at most one run.
* **Atomic settlement and conservation**: per asset,
  `paid out + locked = parked inputs + previously locked`.
* **No double collection**: one `Collect` per allocation, by its receiver only.

### Units

Integer minor units only, never floats. Both assets have 2 decimals. One lot is
1.00 A (`LOT_SIZE_A = 100`). Price is `price_per_lot` in B minor units:
1.20 B per A is `120`. Quote = `lots × price_per_lot`.

## Known limitations (MVP, by design)

* **Maker liveness.** Fills only settle when the maker's node runs the escrow.
  A taker's parked funds wait until the maker's next run (fill, release or
  refund). Mitigation planned: presence handshake before parking, and the
  maker's client running fills automatically while online.
* **Maker-asserted time.** A run's timestamp is the maker's own action
  timestamp, as Unyt's `executed_timestamp` is the executor's. Expiry protects
  the maker, so this is acceptable. If an order expires in the instant between
  `run_escrow` reading the clock and committing, the commit fails validation;
  calling again resolves it.
* **Priority across makers is best-effort.** Time priority is enforced within
  one order. Choosing between orders at the same price is the taker client's job.
* **Makers can omit parks.** A maker chooses which pending parks a run
  consumes. The coordinator always takes the oldest, but validation cannot
  force it.
* **Mint is an open faucet** (capped per call). Test assets only.
* **Chain walks are O(chain length).** Fine for a demo; the real ledger is Unyt.
* **Reads always use the network.** Whether phones (zero-arc nodes) need an
  explicit local/network choice is still an open decision.

## Swapping in real Unyt

The `ledger` coordinator's functions are the whole settlement interface:
`mint`, `open_escrow`, `park`, `run_escrow`, `collect_all`, plus reads. To move
to Unyt, reimplement that surface against Unyt Smart Agreements and remove the
two `ledger` zomes. Things to confirm with Unyt at that point:

1. Can a parked spend be withdrawn before an agreement consumes it?
2. Can one network carry UNIT-A and UNIT-B, and one run move both?
3. How does a script identify who created a parked data record?
4. Can any agent create a Smart Agreement per order, being both spender and
   `AuthorizedExecutor`?
5. Which `rave_engine` version does the network run (needs ≥ 0.7)?
6. Can another hApp call Unyt's zome functions, or only a UI?
7. Is the Unyt DNA bundle available for Sweettest?

## DNA-hash note

Anything under `dnas/dex/zomes/integrity/` and `dnas/dex/dna.yaml` is part of
the DNA hash. Changing them creates a new network. Keep integrity changes in
their own commits, separate from coordinator, UI and test changes.
