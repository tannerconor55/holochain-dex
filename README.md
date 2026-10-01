# DEX MVP — Holochain + (mock) Unyt

A peer-to-peer limit-order exchange for test assets, built on Holochain.
Markets are declared in the DNA properties and every market is quoted in the
hub unit **HF**; the demo DNA declares one market, **A/HF**. Settlement follows the design planned for a Unyt Smart
Agreement, but runs against a **mock Unyt ledger** inside this DNA until Unyt
API access is available.

Spec documents: *DEX MVP Protocol v0.1* and *Smart Agreement Plan v0.1*.

## Status

| Piece | State |
|---|---|
| `dex_core`: settlement logic, order book views, listing tags, order status, market planning, park deadlines, checkpoint arithmetic, markets and DNA properties | Done. 76 unit tests, including a property test of the run-or-reclaim rule over 20,000 generated maker chains. |
| `ledger_integrity` / `ledger` (mock Unyt ledger) | Done. Runs on holochain 0.7.0. Park deadlines and anchored reclaim, balance checkpoints, units and markets from DNA properties. 17 crafted-chain tests run the real `validate` against every attack trace in `docs/design/taker-protection.md`. |
| `dex_integrity`: listing links, validated against the escrow | Done. Wrong tag and wrong author rejected in a conductor. |
| `dex` coordinator: listing, book, take planning, maker settlement, per-market reads, maker presence ping | Done. |
| Signals (`park_placed`, `run_settled` with the recipient's fills, local `order_updated`) and maker auto-run | Done; the UI drives the auto-run. |
| `dex_trades`: trades, 24 h market stats and OHLC candles derived from settlement runs | Done. 10 unit tests. Its own crate so trade logic never touches the DNA hash (see [DNA-hash note](#dna-hash-note)). |
| Trade history externs: `get_recent_trades`, `get_market_stats`, `get_candles` (dex), `get_escrow_trades` (ledger) | Done. Derived on every call from existing runs; nothing stored. See [Trade history and notifications](#trade-history-and-notifications). |
| Notifications: order filled / partial / expired / cancelled, trade settled, settlement failed, park reclaimable, funds waiting on an offline maker | Done. Signals plus poll fallback, deduplicated by stable keys; toasts and the activity log. |
| Market orders (taker-only, IOC, slippage-limited; by lots or budget; one retry) | Done: `dex_core`, `dex` externs, UI. See [Market orders](#market-orders). |
| Sweettest suite | 24 tests pass in ~4 min (each test starts its own conductors; the first build takes ~12 min, see [Build and test](#build-and-test)). Every test ends by checking total supply equals everything minted. |
| UI (`ui/`): wallet, book, limit and market ticket, take flow, my orders, activity, maker presence warning, reclaim countdown and button, market selector, last price and 24 h stats, price chart, recent trades, toasts | Done. 33 Vitest tests (including notification dedupe and the poll fallback against the real store); Playwright runs the demo and a market order against two real conductors and checks trades, stats, chart and a toast. |
| Unyt Smart Agreement template (`unyt/dex_order_escrow`, Rhai) | Done: 4,000 generated runs identical to `dex_core::execute_run` under the published `rave_engine` 0.12.0, park deadlines included. See [Unyt port](#unyt-port). |
| Milestone 2: taker protection, checkpoints, multi-market | Done. Design and proofs: `docs/design/taker-protection.md`. |
| Milestone 3: trade history, price data, reliable notifications | Done. No integrity or `dna.yaml` change; the DNA hash is unchanged. |
| Milestone 4: read performance (day-bucketed listings, 7-day order lifetime, unlisting on close, validated trade index) | Done. At 1000 listed orders: book 7.2 s → 196 ms, stats 10.5 s → 23 ms, candles 10.6 s → 24 ms. Design and benchmark: `docs/design/read-performance.md`. DNA-hash impacting. |

## Layout

```
Cargo.toml                          workspace (zomes + dex_core)
crates/dex_core/                    pure settlement logic, no Holochain deps
crates/dex_trades/                  trades, stats and candles from runs (pure; not linked by integrity)
dnas/dex/dna.yaml                   DNA manifest
crates/ledger_api/, crates/dex_api/ extern input/output types
dnas/dex/zomes/integrity/ledger/    mock Unyt ledger: entry types, validation
  tests/crafted_chains.rs           attack traces against the real validate (native, fake DHT)
dnas/dex/zomes/coordinator/ledger/  mock Unyt ledger: zome functions
dnas/dex/zomes/{integrity,coordinator}/dex/  order book: listings, reads, takes
workdir/happ.yaml                   hApp manifest (role "dex")
tests/sweettest/                    multi-agent conductor tests (own workspace)
unyt/dex_order_escrow/              the settlement run as a Unyt Smart Agreement (Rhai)
tests/rhai_parity/                  template vs dex_core under rave_engine (own workspace)
ui/                                 Svelte UI
docs/design/taker-protection.md     reclaim, checkpoints and markets: design and proof plan
flake.nix                           holonix main-0.7 dev shell
build.sh                            wasm build + dna/happ pack
```

## Build and test

```bash
nix develop                       # holonix 0.7 shell: rust, wasm target, hc, holochain
cargo test -p dex_core            # fast: settlement logic
cargo test -p ledger_integrity    # fast: attack traces against validate
./build.sh                        # zomes -> wasm -> dex.dna -> dex.happ
cargo test --manifest-path tests/sweettest/Cargo.toml   # conductor tests (~5 min)
DEX_BENCH_SIZES=10,100,1000 cargo test --manifest-path tests/sweettest/Cargo.toml \
  bench:: -- --ignored --nocapture                       # read-cost benchmark (~70 min at 1000)
cargo test --manifest-path tests/rhai_parity/Cargo.toml # Rhai template vs dex_core (~80 s)
```

The Sweettest crate builds its dependencies with `opt-level = 3`: the
conductor compiles every zome's wasm with Cranelift on first use, which an
unoptimised `holochain` makes ~30 s per agent. The first build after a clean
takes ~12 minutes; later ones only rebuild the test crate.

**Test environment.** At the default thread count (one per core, 12 here)
the Sweettest suite peaks at about 19 GB, and every conductor keeps its
databases under `TMPDIR`. On a machine whose `/tmp` is a RAM-backed tmpfs
that is memory too: run with `-- --test-threads=6`, or point `TMPDIR` at
disk. A killed or crashed run never deletes its conductors' data, so it
piles up in `/tmp/nix-shell.*` (one per `nix develop`) and later runs fail
with "Disk quota exceeded" or are OOM-killed: delete the stale
`nix-shell.*` directories (those no running process uses) after a killed
run.

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
5. **Reclaim.** A park its maker has not settled by its deadline can be taken
   back by the taker with a `Reclaim`, once the maker has written any action
   at or after that deadline (see [Taker protection](#taker-protection)).

Every validator re-executes the run with `dex_core::execute_run` and requires
an exact match, like peers re-running a Unyt RAVE.

### Invariants enforced by validation

* **No negative balances**: `Escrow` and `Park` debits are checked against the
  author's balance: their latest `Checkpoint` plus the ledger entries after
  it, never the whole chain.
* **No double-spend of an escrow**: runs are maker-only, and each must name the
  maker's latest run for that escrow as `prev_run`, checked by walking the
  maker's chain back to it. A park can be consumed by at most one run.
* **Run or reclaim, never both**: a run may consume a park only before the
  park's deadline; a `Reclaim` must cite a maker action at or after the
  deadline, and the maker's chain from the escrow to that action must hold no
  run consuming the park. On an unforked chain a park is therefore paid out
  once; a double spend through a fork gets the maker warranted by Holochain.
* **Atomic settlement and conservation**: per unit,
  `paid out + locked = parked inputs + previously locked`.
* **No double collection**: one `Collect` per allocation, by its receiver
  only, across checkpoints (a checkpoint carries every allocation collected).
* **Bounded validation cost**: a checkpoint must equal the previous one plus
  the entries since; a debit, collect or reclaim citing a checkpoint that is
  not the latest, or more than 256 actions old, is refused. The coordinator
  writes one every 32 ledger entries or 128 actions.
* **Declared units and markets only**: amounts in an undeclared unit, a park
  in another market than its escrow, and a price off the market's tick are
  refused.
* **Listings are bucketed and bounded**: a listing hangs off its market's
  anchor for its escrow's creation day (UTC), and the escrow must expire at
  most `max_order_lifetime_secs` after it was created. Only its maker may
  delete a listing.
* **The trade index cannot lie**: a trade index link must target a run by its
  own author, hang off the anchor for its market and the run's day, and carry
  exactly the run's price, side, timestamp, escrow and the lots it sold; index
  links cannot be deleted.

### Units, markets and timing (DNA properties)

`dnas/dex/dna.yaml` declares, and `genesis_self_check` and `validate` check
(missing or malformed properties are refused, never defaulted):

| Property | Demo DNA | Meaning |
|---|---|---|
| `units` | `A` and `HF`, 2 decimals each | every amount is in integer minor units of a declared unit |
| `markets` | `A/HF`, `lot_size` 100, `tick_size` 1 | base/quote (quote is always `HF`); a market's id is `blake2b-256(base, quote)`; duplicates, `base == quote`, a pair and its inverse, and zero lot or tick are refused at genesis |
| `park_timeout_secs` | 1800 | a park's run window |
| `settle_grace_secs` | 300 | added after the window (or after the order's expiry, if sooner) |
| `max_order_lifetime_secs` | 604800 (7 days) | the longest an order may be listed for; `place_order` refuses a later expiry before locking any funds |

Price is `price_per_lot` in quote minor units: 1.20 HF per A is `120`, a
multiple of `tick_size`. One lot is `lot_size` base minor units (100 =
1.00 A). Quote = `lots × price_per_lot`. Each market has its own book (one
anchor per market); the UI's market selector appears once a second market
is declared. Properties are part of the DNA hash: changing any value makes a
new network.

## Taker protection

A park's **deadline** is `min(parked_at + park_timeout, expires_at) +
settle_grace`. The maker's coordinator only settles parks with time to
spare before it (60 s, or a fifth of the window if shorter).

* **Reclaim.** After the deadline the taker's UI counts down to "Reclaim";
  `reclaim_park` finds the maker's latest action and, if it is stamped at or
  after the deadline, writes a `Reclaim` citing it. Validation walks the
  maker's chain from the escrow to that action and refuses the reclaim if any
  run there consumed the park.
* **Why an anchor is needed.** Holochain only requires an author's
  timestamps to be non-decreasing along their own chain; nothing bounds how
  far back an action may be stamped. Without a later maker action to walk to,
  validation cannot rule out a run the maker has yet to write, stamped before
  the deadline.
* **Maker presence (advisory).** Before a take, the UI pings each maker
  (`check_makers`, all at once, under a cap grant for `ping` only) and by
  default refuses to park against one not seen in the last 60 s, with a
  "Park anyway" override. It proves nothing about later, and validation does
  not depend on it.

The proof is in three layers (design doc section 9): a `dex_core` property
test of the rule; crafted-chain tests that feed backdated chains to the real
`validate` (every trace T1–T13, including the maker backdating a run as far
as Holochain allows); and Sweettests on real conductors (a maker offline past
the deadline, early, foreign and repeated reclaims, a settled park).

## Trade history and notifications

**Trades are derived, not stored.** A `Fill` run's lock shrinks by exactly
the lots it filled, all at the order's price, so each such run is one trade;
a `Release` run fills nothing. `dex_trades` turns an order's runs into
trades, the last price and 24 h stats (open, change, high, low, lot and
quote volume, count), and OHLC candles on epoch-aligned intervals (1 min to
1 day, at most 1,000 per request). The ledger exposes one read,
`get_escrow_trades`, so the dex zome still never reads ledger entries.

**Where reads come from (milestone 4).** Listings hang off per-market,
per-day anchors (the escrow's UTC creation day), and an order lives at most
7 days, so the book reads the 8 days that can still hold a live order and
checks each listed order's state. A maker deletes an order's listing when a
run closes it (filled, cancelled, or released after expiry), so those days
hold mostly open orders. After every run that sells lots the maker writes a
trade index link under the market's anchor for the run's day; validation
checks its tag against the run, so recent trades, stats and candles read
only the index (one `get_links` per day: 2 for the 24 h stats, up to 8 for a
7-day chart) and never a run. Recent trades and the last price look back 30
days; a candle request spans at most 31 days.

**Notifications** come two ways and are shown once. Signals are fast but may
be lost: each receiving taker's `run_settled` carries how their parks came
out, and the maker's UI gets a local `order_updated` after each run. The
10 s poll derives the same events from state changes (plus what no action
announces: a park becoming reclaimable, funds waiting on a maker who does
not answer a ping, a failed settlement or collection). Both paths build the
same key from stable ids (park, escrow and status and filled lots, ...), so
a missed signal only delays a notice. The first poll after loading records
history silently (no replay) but still shows what needs action now.

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
* **Amount:** whole lots, or a budget of the paying asset (HF to buy, the base unit to sell)
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

* **Maker liveness.** A park the maker has not settled within its deadline
  (30 minutes after parking, or the order's expiry if sooner, plus 5 minutes'
  grace) can be reclaimed by the taker, and the maker's cooperation is not
  needed: any action the maker writes on this network after the deadline
  (settling another order, collecting, trading) is enough to prove the park
  was never settled. If the maker never writes anything again, the park
  cannot be reclaimed: validation has no clock, and Holochain lets an author
  backdate an action to their own last action, so only a later maker action
  can prove no settlement is still to come. The maker's own escrowed funds
  stay locked in the same way.
* **Forks.** A maker who forks their chain can make both a run and a reclaim
  of one park valid, each on its own branch. Holochain detects the fork and
  warrants the maker; validation reads one branch and cannot prevent it. The
  escrow lock has the same exposure.
* **Presence means the conductor, not the app.** The maker ping is answered
  by the maker's conductor, but orders settle only while the maker's app is
  open (the UI drives the auto-run). Under hc-spin or a launcher the two run
  together; with a separately running conductor the check can pass while
  nothing settles.
* **Signals can arrive before the data.** A run's signal can reach the taker
  before their node sees the run: the toast ("Trade settled") comes first,
  and the take panel, wallet and My takes catch up on a later poll (within
  10 s once gossip delivers the run).
* **The book read still grows mildly with absent makers.** Expired orders
  whose maker never comes back to release them are never unlisted; the book
  fetches their links (skipping them by their tag's expiry, without a state
  read) until they fall out of the 7-day window. At 1000 orders listed in one
  day, 330 of them expired and never released, the book read is 196 ms
  (47 ms with 10). The 7-day lifetime bounds it.
* **A skipped trade-index link hides that trade.** The maker writes the index
  link; a maker whose client does not (or settles through the ledger
  directly) leaves the trade out of recent trades, stats and candles. This is
  informational only: settlement, balances and validation never read the
  index.
* **Price history looks back 30 days.** A market with no trade in 30 days
  shows its last price as unknown.
* **Writing a run grows with the maker's history.** Settling or cancelling
  an order cost 0.6–0.8 s with 100 orders of history and 5–6 s with 1000 in
  the (adversarial) benchmark; placing an order stays cheap. A follow-up
  splits validation from coordinator cost.
* **Price data refreshes with the book** (every 10 s) and on settlement
  signals; a trade from an order the reader has no signal for can take up
  to 10 s to show.
* **Trade timestamps are the maker's.** A trade's time is its run's action
  timestamp, which the maker asserts (as Unyt's `executed_timestamp` is the
  executor's); a maker with a wrong clock misplaces their trades on the
  chart. Validation only guarantees timestamps never go backwards on a chain.
* **Reclaim becomes available eventually.** After the maker's anchoring
  action, the taker's node may take a few seconds to see it (the UI polls
  every 10 s).
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
* **The collected set grows.** A checkpoint carries every allocation its
  author ever collected (about 40 bytes each), so checkpoints grow with an
  account's history; 100,000 collects is about the 4 MB entry limit. Design
  doc section 7 records the alternatives.
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
| Differential: `dex_core`'s own generator (same seed and draws), 2,000 cases, each as an opening run and as a later run, with park deadlines straddling the run time | 4,000 runs identical: allocations per receiver, locked, consumed order, per-park fills, rejected parks (past their deadline or beyond the cap, in time order); 159,230 lots filled, 9,919 parks past their deadline |
| From half-sold locks | 500 more runs identical |
| Every `dex_core` unit-test scenario, both starts where possible | identical, including §30 chained through the engine's own output, the cap and the deferred park's next run, and the same refusals (duplicate park, bad lock, invalid terms) |
| Conservation per unit and source naming, checked on every Rhai output | hold |
| Formatter and parsers, run from the shipped script | exact 2 decimals, no `-0`, `i64::MAX`, round trips; RFC3339 with 0/3/6 fractional digits |
| Park deadlines | at the deadline vs 1 µs before, in Fill and Release; no declared deadline; expired parks outside the 20-park cap |
| Mutation check | an expiry off-by-one, a reversed tie-break, and ignoring park deadlines each fail |

**Operations** (binary search on `max_operations`, default budget 100,000):

| Run | Operations |
|---|---|
| no parks | 976 |
| 1 park | 1,888 |
| 20 parks (the cap) | 13,681 (14% of the budget; 635 per park) |
| opening run with 20 parks | 13,767 |
| 25 parks, 5 deferred | 14,056 |

The deadline is **taker-declared** in the spend payload (`park_deadline`,
beside `requested_lots`), since no helper returns a parked link's timestamp
(question 13). A wrong one only affects the taker's own spend.

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
- That `ProvidedBy: taker_spender` inputs named `requested_lots` and
  `park_deadline` are filled from each spend's payload (`ParkedLink::get_input_for_key` is published;
  its caller is not).
- Which links a run is handed, in what order, and how many.
- `AuthorizedExecutor` enforcement, the refusal of a lock under `Any`, and
  the re-run on validation.
- Whether `runtime_input_signature.json` is applied to recorded inputs.

## Swapping in real Unyt

The `ledger` coordinator's functions are the whole settlement interface:
`mint`, `open_escrow`, `park`, `run_escrow`, `collect_all`, `reclaim_park`,
plus reads (and test-only `*_raw` writes). To move
to Unyt, reimplement that surface against Unyt Smart Agreements built from
`unyt/dex_order_escrow` and remove the two `ledger` zomes. Questions for Unyt,
with what the port answered:

1. Can a parked spend be withdrawn before an agreement consumes it? *Open.*
   The mock ledger's anchored reclaim is the behaviour takers need; the
   template refuses to consume a park past its deadline, but cannot give it
   back.
2. Can one network carry several units (A, HF, …), and one run move two? *Partly
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
13. *New:* is there, or could there be, a helper that returns a parked link's
    timestamp? `acceding_sort_allocation` sorts by it but does not return it,
    so the template relies on a taker-declared deadline in the spend payload.
    The template can refuse expired parks; a taker's reclaim on Unyt depends
    on question 1.
14. *New:* how are credit limits set and enforced for a hub currency (HF),
    and who may change them?
15. *New:* can a `ParkedSpendCredit` (spending on credit) fund a DEX order
    escrow, or only `ParkedSpendBalance`?
16. *New:* how can a DEX integrity zome validate a trade index link that
    points at a RAVE in the Unyt DNA? Validation cannot read across DNAs, so
    the mock's check (the tag equals the run's trade) would need a Unyt-side
    helper or a Unyt-side index; without one, the index on Unyt would be
    maker-attested and unvalidated.

## DNA-hash note

Anything under `dnas/dex/zomes/integrity/` and `dnas/dex/dna.yaml`
(including its `properties`: units, markets, timing) is part of the DNA hash.
Changing them creates a new network. The crafted-chain tests under
`integrity/ledger/tests/` are not compiled into the wasm and do not.

So does any change to `crates/dex_core` or `crates/ledger_api`: both
integrity zomes link them, and even an unused new module changes their
wasm. Logic validation never needs goes in its own crate (`dex_trades`).
Check with `sha256sum target/wasm32-unknown-unknown/release/*_integrity.wasm`
before and after `./build.sh`, or compare `hc dna hash dnas/dex/workdir/dex.dna`. Keep integrity changes in
their own commits, separate from coordinator, UI and test changes.
