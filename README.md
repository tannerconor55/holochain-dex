# DEX MVP — Holochain + (mock) Unyt

A peer-to-peer limit-order exchange for two test assets, **UNIT-A / UNIT-B**,
built on Holochain. Settlement follows the design planned for a Unyt Smart
Agreement, but runs against a **mock Unyt ledger** inside this DNA until Unyt
API access is available.

Spec documents: *DEX MVP Protocol v0.1* and *Smart Agreement Plan v0.1*.

## Status

| Piece | State |
|---|---|
| `dex_core`: settlement logic, order book views, listing tags, order status, market planning | Done. 48 unit tests. |
| `ledger_integrity` / `ledger` (mock Unyt ledger) | Done. Runs on holochain 0.7.0. |
| `dex_integrity`: listing links, validated against the escrow | Done. Wrong tag and wrong author rejected in a conductor. |
| `dex` coordinator: listing, book, take planning, maker settlement | Done. |
| Signals (`park_placed`, `run_settled`) and maker auto-run | Done; the UI drives the auto-run. |
| Market orders (taker-only, IOC, slippage-limited; by lots or budget; one retry) | Done: `dex_core`, `dex` externs, UI. See [Market orders](#market-orders). |
| Sweettest suite | 16 tests pass (~20 min; each test starts its own conductors). |
| UI (`ui/`): wallet, book, limit and market ticket, take flow, my orders, activity | Done. 16 Vitest tests; Playwright runs the demo and a market order against two real conductors. |
| Unyt Smart Agreement template (`unyt/dex_order_escrow`, Rhai) | Done: 4,000 generated runs identical to `dex_core::execute_run` under the published `rave_engine` 0.12.0. See [Unyt port](#unyt-port). |

## Layout

```
Cargo.toml                          workspace (zomes + dex_core)
crates/dex_core/                    pure settlement logic, no Holochain deps
dnas/dex/dna.yaml                   DNA manifest
crates/ledger_api/, crates/dex_api/ extern input/output types
dnas/dex/zomes/integrity/ledger/    mock Unyt ledger: entry types, validation
dnas/dex/zomes/coordinator/ledger/  mock Unyt ledger: zome functions
dnas/dex/zomes/{integrity,coordinator}/dex/  order book: listings, reads, takes
workdir/happ.yaml                   hApp manifest (role "dex")
tests/sweettest/                    multi-agent conductor tests (own workspace)
unyt/dex_order_escrow/              the settlement run as a Unyt Smart Agreement (Rhai)
tests/rhai_parity/                  template vs dex_core under rave_engine (own workspace)
ui/                                 Svelte UI
flake.nix                           holonix main-0.7 dev shell
build.sh                            wasm build + dna/happ pack
```

## Build and test

```bash
nix develop                       # holonix 0.7 shell: rust, wasm target, hc, holochain
cargo test -p dex_core            # fast: settlement logic
./build.sh                        # zomes -> wasm -> dex.dna -> dex.happ
cargo test --manifest-path tests/sweettest/Cargo.toml   # conductor tests
cargo test --manifest-path tests/rhai_parity/Cargo.toml # Rhai template vs dex_core (~75 s)
```

Keep `tests/sweettest/Cargo.lock`: it is seeded from holochain 0.7.0's own
published `Cargo.lock`, with this repo's crates added on top, so the
conductor builds against exactly the dependency versions Holochain released
with. On 0.6.3 a fresh resolve once broke `ed25519-dalek` inside the iroh
transport; treat any lockfile regeneration as a change to test.

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

# More agents: either launcher takes AGENTS (sandbox: up to 10, named
# alice, bob, carol, …; app ports 8801, 8802, …).
AGENTS=3 nix develop .. -c npm start
AGENTS=3 nix develop -c ./scripts/sandbox.sh

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

## Market orders

A market order is a **taker action only**: the client plans a sweep of resting
orders from the best price and parks against each through the same path as a
limit take. There is no market-order entry, no ledger change and no
validation change.

* **Immediate-or-cancel.** A market order never rests on the book. Whatever the
  makers' runs cannot fill (someone else got there first, the order expired) is
  refunded by those runs.
* **Fills settle per maker.** Each maker's client runs its own order, at the
  maker's own limit price, when it next runs (on the `park_placed` signal or its
  poll). A sweep across three makers settles in up to three separate steps and
  can arrive partially, not all at once.
* **Slippage is a client-side limit.** `max_slippage_bps` (default 200 = 2%)
  turns the best price the taker can hit (live orders, not their own) into a
  limit: buy `ceil(best × (10000 + bps) / 10000)`, sell
  `floor(best × (10000 − bps) / 10000)`, integer maths only. The limit decides
  which orders the taker parks against; validation knows nothing about it and
  does not enforce it. Note that these roundings loosen the limit by up to one
  minor unit per lot (a buy at best 1.20 with 2% may pay up to 1.23, not 1.224).
* **Cross-maker priority is chosen by the taker's client**, best price then
  oldest, the same as a limit take. Nothing forces another client to do the same.
* **Amount:** whole lots, or a budget of the paying asset (B to buy, A to sell)
  spent on whole lots without ever exceeding it. The plan reports per-order
  lots and cost, the average price as an exact rational
  (`total_quote_minor / total_lots`), the worst price and any shortfall.
* **One retry.** `retry_market_shortfall` re-plans only the lots known to be
  unfilled (the original shortfall plus refunded parks) against the fresh
  book, within the **original** limit price, never one derived from the moved
  book. A retry cannot itself be retried; the UI offers it once, after every
  maker has settled.

Externs: `preview_market_order`, `market_order`, `market_order_by_budget`,
`retry_market_shortfall`. Planning: `dex_core::book::market`.

## Known limitations (MVP, by design)

* **Maker liveness.** Fills only settle when the maker's node runs the escrow.
  A taker's parked funds wait until the maker's next run (fill, release or
  refund). The maker's client settles automatically while its app is open;
  a maker who closes the app leaves takers waiting until they return.
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
* **Reads always use the network.** v1 targets desktop full-arc nodes only; phones
  (zero-arc nodes) would need an explicit local/network choice on every read.

## Unyt port

`unyt/dex_order_escrow/` is one settlement run as a Unyt Smart Agreement
template: Rhai execution code plus its schemas, roles and an example
(never-instantiated) agreement. One agreement instance per order, executed
by the maker. Its README covers roles, inputs, outputs and trust
assumptions. `tests/rhai_parity/` runs it through the published
`rave_engine` crate (pinned `=0.12.0`, built against `hdk 0.7` / `hdi 0.8`,
the same line as this repo) and compares every run with
`dex_core::execute_run`.

Tags: **[engine]** is shown by running the published crate; **[DNA]** can
only be checked on a Unyt network, because the validator is not public.

### Results

| Check | Result |
|---|---|
| Differential: `dex_core`'s own generator (same seed and draws), 2,000 cases, each as an opening run and as a later run | 4,000 runs identical: allocations per receiver, locked, consumed order, per-park fills, deferred parks (186,180 lots filled) |
| From half-sold locks | 500 more runs identical |
| Every `dex_core` unit-test scenario, both starts where possible | identical, including §30 chained through the engine's own output, the cap and the deferred park's next run, and the same refusals (duplicate park, bad lock, invalid terms) |
| Conservation per unit and source naming, checked on every Rhai output | hold |
| Formatter and parsers, run from the shipped script | exact 2 decimals, no `-0`, `i64::MAX`, round trips; RFC3339 with 0/3/6 fractional digits |
| Mutation check | an expiry off-by-one and a reversed tie-break each fail a scenario |

**Operations** (binary search on `max_operations`, default budget 100,000):

| Run | Operations |
|---|---|
| no parks | 964 |
| 1 park | 1,849 |
| 20 parks (the cap) | 13,129 (13% of the budget; 608 per park) |
| opening run with 20 parks | 13,215 |
| 25 parks, 5 deferred | 13,369 |

### What is proven, and what only Unyt can confirm

Proven by running the published engine [engine]:
- The script compiles and runs under `rave_engine` 0.12.0 with its limits,
  and its output parses as a `RAVEOutput`: unit-map strings, name-only
  allocations (`amounts: {}`), two units in one allocation, `rejected_links`.
- Priority comes from the real `acceding_sort_allocation`, fed parked-link
  records by a mocked host (its stable timestamp sort, with ties broken by a
  source-hash presort in the script).
- Given the inputs the DNA is documented to pass, every output equals
  `dex_core::execute_run`, and per-unit conservation holds by the harness's
  own check.

Only checkable on a Unyt network [DNA]:
- Conservation per unit, and that a source counts once an allocation names it.
- That a `ProvidedBy: taker_spender` input named `requested_lots` is filled
  from each spend's payload (`ParkedLink::get_input_for_key` is published;
  its caller is not).
- Which links a run is handed, in what order, and how many.
- `AuthorizedExecutor` enforcement, the refusal of a lock under `Any`, and
  the re-run on validation.
- Whether `runtime_input_signature.json` is applied to recorded inputs.

## Swapping in real Unyt

The `ledger` coordinator's functions are the whole settlement interface:
`mint`, `open_escrow`, `park`, `run_escrow`, `collect_all`, plus reads. To move
to Unyt, reimplement that surface against Unyt Smart Agreements built from
`unyt/dex_order_escrow` and remove the two `ledger` zomes. Questions for Unyt,
with what the port answered:

1. Can a parked spend be withdrawn before an agreement consumes it? *Open.*
2. Can one network carry UNIT-A and UNIT-B, and one run move both? *Partly
   answered:* a unit map holds several indexes and one allocation carries both
   [engine]. Open: per-unit conservation [DNA], and that both units allow at
   least 2 decimals (amounts are written as `"123.45"`).
3. How does a script identify who created a parked data record? *Answered:*
   `get_spend_links_author` resolves any parked link, data links included
   [engine]. The template avoids needing it: `requested_lots` rides in the
   taker spend's own payload.
4. Can any agent create a Smart Agreement per order, being both spender and
   `AuthorizedExecutor`? *Open.*
5. Which `rave_engine` version does the network run? *Open.* The template is
   built and tested on 0.12.0 (Holochain 0.7); it needs at least 0.7 and
   relies on 0.11's deferring of host fetch errors.
6. Can another hApp call Unyt's zome functions, or only a UI? *Open.*
7. Is the Unyt DNA bundle available for Sweettest? *Open;* it would turn every
   [DNA] item above into a test.
8. *New:* does the DNA fill a `ProvidedBy` input named `requested_lots` from the
   spend link's payload? If not, the fallback is a `ParkedData(true)` request
   role paired by author.
9. *New:* in what order, and how many at most, are a role's parked links
   handed to an aggregate run? The script sorts them itself; a cap below 20
   would matter.
10. *New:* are links returned in `rejected_links` offered to the next run?
    (They stay on chain [engine]; the template relies on the next run seeing them.)
11. *New:* must a carried lock be named as a source when an allocation draws
    on it, or is it inherited? The template names it (`previous_execution.id`),
    as `lockbox` does.
12. *New:* can the maker park the opening spend targeted at themselves as the
    executor, and does `GetPreviousExecution` see nothing on that first run?

## DNA-hash note

Anything under `dnas/dex/zomes/integrity/` and `dnas/dex/dna.yaml` is part of
the DNA hash. Changing them creates a new network. Keep integrity changes in
their own commits, separate from coordinator, UI and test changes.
