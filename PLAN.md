# PLAN.md — order book zomes and UI

Working plan for Claude Code. Read `CLAUDE.md` and `README.md` first. State at
start: `dex_core` (18 unit tests) and the mock `ledger` zomes (4 Sweettest
tests) pass. Nothing exists yet for order discovery, price levels, signals or UI.

Use the `holochain`, `holochain-dev` and `unyt-smart-agreement` skills. Verify
every HDK/HDI call against the pinned crate source (`hdk 0.7.0`, `hdi 0.8.0`; upgraded from 0.6.3 before Phase 3);
items marked **(verify)** below are things I have not checked.

## Decisions made (change them only deliberately)

| Question | Decision | Why |
|---|---|---|
| Where is order discovery stored? | **A link, not a new entry.** `Market anchor → escrow`, with a tag encoding side, price and expiry. | The escrow already holds the terms. A second `Order` entry would duplicate them and could disagree. |
| Can a listing lie about price? | No. Integrity validation recomputes the tag from the escrow's terms and requires link author = escrow author. | Prices in the book are then as trustworthy as the escrow. |
| Where does remaining quantity come from? | Only from the ledger: `get_escrow_state` (the latest run's `locked`). | One source of truth (§23). |
| Book maths (aggregation, take planning) | Pure functions in `dex_core::book`, unit tested. | Same rule as settlement: no logic forked into zomes or UI. |
| Zero-arc / phones? | **No** for v1 (desktop, full-arc; confirmed 2026-09-29). Reads use `GetStrategy::Network`. | If that changes, add a `local` flag to every read. |
| Relaxed / async commits? | **No** for v1 (synchronous; confirmed 2026-09-29). Signals are sent inline from externs. | If that changes, move signals to `post_commit`. |
| Who settles fills? | The maker's own client, automatically while it is online. | Protocol design: `AuthorizedExecutor` = maker. |
| UI stack | Svelte 5 + Vite + TypeScript + `@holochain/client`. | Matches what the scaffolder produces. Use `@holochain/client` 0.21 (targets Holochain 0.7); keep a single copy in the tree. |
| Multi-agent local dev | Try `hc-spin` in the Holonix shell **(verify it exists)**; otherwise two `hc sandbox` instances on different ports and a `?app_port=` URL param. | Every demo needs two agents. |

## Phase 0 — shared API crate (refactor, no behaviour change)

The ledger coordinator defines its input/output structs (`ParkRequest`,
`EscrowState`, `RunReport`, `BalanceView`, ...). The dex coordinator and the
Sweettest crate both need them, and the Sweettest crate currently keeps
hand-written mirrors. Depending on the `ledger` crate directly would duplicate
its `#[hdk_extern]` exports.

- Create `crates/ledger_api` with those structs. Depend on `hdi` (for
  `ActionHash`, `AgentPubKey`, `Timestamp`) and `serde` only, not `hdk`.
- Use it from `ledger`, the new `dex` coordinator and `tests/sweettest`; delete
  the mirrors.
- **Accept when:** all existing tests pass unchanged. Not DNA-impacting (coordinator only).
- **Commit:** `Extract ledger_api crate`.

## Phase 1 — `dex_core::book` (pure logic)

Add `crates/dex_core/src/book.rs`, with tests in the same style as `tests.rs`.

```rust
pub struct OrderView<P> {
    pub id: P,
    pub side: Side,
    pub price_per_lot: u64,
    pub remaining_lots: u64,
    pub opened_at: i64,   // µs; escrow action timestamp
    pub expires_at: i64,
}
pub struct PriceLevel { pub price_per_lot: u64, pub lots: u64, pub orders: usize }
pub struct BookView { pub asks: Vec<PriceLevel>, pub bids: Vec<PriceLevel>, pub spread: Option<u64> }

pub fn live(orders, now) -> impl Iterator      // drops remaining==0, expired, closed
pub fn aggregate<P>(orders: &[OrderView<P>], now: i64) -> BookView
pub fn orders_at_level<P>(orders, side, price, now) -> Vec<OrderView<P>>   // time priority
pub fn plan_take<P>(orders, take: Side, lots: u64, limit_price: Option<u64>, now) -> TakePlan<P>
```

Rules to encode and test:

- Asks sorted ascending by price, bids descending. Spread = best ask − best bid,
  `None` if either side is empty, and shown as a crossed book (negative) never
  silently clamped: use `i64` or an explicit `Crossed` variant.
- Within a price: `(opened_at, id)` ascending, the same tiebreak as `execute_run`.
- `plan_take` walks best price first, then time. **Taker buys** consume asks;
  **taker sells** consume bids. It stops at `lots` or at `limit_price`, and
  returns `filled`, `shortfall`, per-order `lots`, and the exact `cost` per
  order using `OrderTerms` math (`lots × taker_units_per_lot`). Never floats.
- A taker's own orders are skipped (a self-trade is legal in the ledger but
  pointless in the UI).
- Tests: level merging (§10 example: 40+35+25 at 1.20 = 100), the §13 example
  (60 A consumes Alice 40 then Bob 20), expired and empty orders excluded,
  limit price respected, shortfall reported, crossed book, buy/sell symmetry,
  a randomised check that `plan_take` never exceeds a level's lots or the
  request.
- **Accept when:** `cargo test -p dex_core` passes. **Commit:** `Add dex_core::book`.

## Phase 2 — `dex` integrity zome (DNA-hash impacting: own commit)

New crates `dnas/dex/zomes/integrity/dex` (`dex_integrity`) and
`dnas/dex/zomes/coordinator/dex` (`dex`). Register both in `Cargo.toml`,
`dna.yaml` and `build.sh`.

Integrity:

- No entry types (keep one placeholder only if the tooling requires it; do not
  delete it later as "useless") **(verify whether a zome with zero entry types is allowed)**.
- Link types: `MarketToOrders` (anchor → escrow). Base: a path anchor for the
  single market `unit_a_unit_b`. Use `hdk::hash_path` typed paths, which need
  their own `Anchor` link type **(verify the current typed-path API)**.
- Tag: `side (1 byte) | price_per_lot (u64 big-endian) | expires_at (i64 big-endian)`.
  Big-endian so byte-prefix filters give price ordering. One `encode_tag` /
  `decode_tag` in a shared spot, used by both integrity and coordinator so the
  encoder and decoder can never diverge.
- `validate_create_link` for `MarketToOrders`:
  1. target is an action hash; fetch it with `must_get_valid_record`;
  2. its entry is a `ledger_integrity::Escrow` (decode the app entry bytes with
     `TryFrom<SerializedBytes>`, not by trial across entry types) **(verify:
     decoding another integrity zome's entry from inside this zome)**;
  3. `action.author == escrow_record.author`;
  4. `tag == encode_tag(escrow.terms)`;
  5. base is the market anchor.
- Links are permanent: reject `DeleteLink`. Cancelled, expired and filled
  orders leave the book because the coordinator filters on ledger state, not
  because links are removed.
- **Accept when:** `cargo check --workspace` clean. **Commit:** `Add dex integrity zome (DNA-hash impacting)`.

## Phase 3 — `dex` coordinator

All settlement calls go through the `ledger` zome with
`call(CallTargetCell::Local, ZomeName::from("ledger"), ...)`. The dex zome never
reads or writes ledger entries directly, so the ledger stays swappable for Unyt.
Check every `ZomeCallResponse` variant exhaustively.

| Extern | Does |
|---|---|
| `place_order(terms)` | `ledger.open_escrow`, then `create_link(anchor, escrow, MarketToOrders, encode_tag(terms))`. Returns the escrow hash. If the link step fails after the escrow exists, the order is real but undiscoverable: return the escrow hash with a warning and let `republish_listing(escrow)` retry. |
| `get_order_book()` | Fetch anchor links, prefilter on the tag, fetch `ledger.get_escrow_state` for each, build `OrderView`s, run `dex_core::book::aggregate`. |
| `get_level_orders({side, price})` | Underlying orders at one level, in priority order. |
| `plan_take({take, lots, limit_price})` | `dex_core::book::plan_take`. Read-only. |
| `take(plan)` | For each planned order: `ledger.park` with exact funds, then a remote signal to that order's maker (see Phase 4). Returns park hashes. Re-validates the plan against fresh state and returns what changed, so a stale UI never parks against a filled order. |
| `cancel_order(escrow)` | `ledger.run_escrow(Release)`, repeated while `still_pending > 0`. |
| `run_my_orders()` | For each of my escrows: expired or cancel-requested → Release loop; otherwise Fill if parks are pending. Idempotent. |
| `my_orders()` | My escrows with state, pending parks, status label (Open / Partial / Filled / Cancelled / Expired). |
| `my_parks()` | Parks I placed and how they resolved (pending / filled lots / refunded). |

Design notes:

- N+1 reads: the book fetches state per order. Fine at MVP scale. Record the
  cost and leave a comment where a cache or a maker-published summary would go.
- The book is a **view**: nothing computed here is stored (§10).
- Status label logic lives in `dex_core::book` (`OrderStatus`), not the UI.
- Expiry uses `sys_time()`. Near the boundary a run can fail validation and
  need one retry (already noted in the README); `run_my_orders` should retry once.
- **Accept when:** new Sweettest cases pass (see Phase 5). **Commit:** `Add dex coordinator`.

## Phase 4 — signals and maker auto-run

- `init`: grant an unrestricted cap for `recv_remote_signal` only **(verify the
  HDK 0.7.0 init cap-grant pattern)**.
- `take` sends `Signal::ParkPlaced { escrow, park }` to the maker with
  `send_remote_signal`. `run_escrow` results send `Signal::RunSettled { run }`
  to every allocation receiver so they can collect.
- `recv_remote_signal` re-emits to the local UI with `emit_signal`.
- Signals are hints, never state. The UI must also poll (every ~10 s), so a
  missed signal only delays a fill and never loses one.
- **Maker liveness**: the UI runs `run_my_orders` on every `ParkPlaced` signal
  and on the poll. Document that a maker who closes the app leaves takers'
  funds parked until they reopen (existing README limitation; the UI should
  show a "maker seen recently" hint if cheap).
- **Accept when:** a two-conductor Sweettest shows a park causing a fill without a
  manual `run_escrow` call once `run_my_orders` is invoked, and unit tests cover
  the signal payload shapes. **Commit:** `Add signals and maker auto-run`.

## Phase 5 — Sweettest additions

Run `./build.sh` first. Reuse the `TestEnv` helper; use `ledger_api` types.
Always `await_consistency` before a cross-agent read.

1. Order book shows a level after `place_order`, hides it after full fill,
   cancel and expiry.
2. Three makers at one price: `get_level_orders` returns time priority.
3. `plan_take` then `take` across two orders fills both; balances conserve.
4. Listing with a wrong tag is rejected (write a link by hand through the
   ledger-independent path).
5. A listing created by someone other than the escrow's maker is rejected.
6. Ledger edge cases still open from before: fill with nothing pending, park on
   an expired order, 21 pending parks (cap, then the remainder on the next run),
   and a run that omits the oldest park (document the observed behaviour; the
   README already states validation cannot force inclusion).
7. Total supply check: sum of all agents' `BalanceView.total` equals total minted
   after every scenario.

Tests take ~3 minutes today. If it hurts, share one conductor setup between
tests before adding more.

## Phase 6 — UI

Location: `ui/` (own `package.json`, not part of the Cargo workspace).

**Setup**

- Svelte 5 + Vite + TypeScript. `@holochain/client` 0.21 (Holochain 0.7; keep one copy via `overrides`).
  Node 22 is already in the Nix shell.
- `AppWebsocket.connect({ url, token })` for the app port; get a token from the
  admin websocket with `issueAppAuthenticationToken`, and authorize the signing
  credentials **(verify current `@holochain/client` connect flow)**. Read the port
  from `?app_port=` so two browser tabs can be two agents.
- One typed `api.ts` wrapping `callZome`, with TS types hand-mirrored from
  `ledger_api` and the dex externs. Keep them in one file so drift is visible.
  Amounts stay integer minor units end to end; format only at display
  (`formatAmount(minor) → "1.20"`). u64 fits a JS number up to 2^53, far above
  the mint cap; timestamps (µs) are also below 2^53. Add a runtime assert.
- **Serialization errors**: rebuild the wasm and repack first, then compare the TS
  and Rust types. Never bump msgpack or serde versions as a first response.

**Screens** (one page, three columns on desktop, stacked on narrow screens)

1. **Header / wallet.** Agent short key, balance card: available, locked in my
   orders, parked, uncollected, total (§23; show they add up). "Get test funds"
   button (`mint`). Uncollected is collected automatically on `RunSettled` and
   on the poll; a manual Collect button remains.
2. **Order book.** Asks on top (descending toward the spread), spread row, bids
   below, each level `price · lots · order count`, with a depth bar. Updates on
   signal and every ~10 s. Empty-state and loading-state handled, and a partial
   result never blanks the book (eventually consistent reads can miss items).
3. **Order ticket.** Buy/Sell toggle, price (B per A, 2 dp), quantity (A, whole
   lots), expiry (15 min / 1 h / 24 h). Live preview: "You lock 100.00 A"
   or "You lock 120.00 B", validated against available balance. Submit calls
   `place_order`.
4. **Take flow.** Clicking a level opens a panel prefilled with that price.
   User enters quantity, the UI calls `plan_take` and shows the orders it will
   hit, per-order lots, total cost, and any shortfall. Confirm calls `take`.
   Then show live status per park: waiting for maker → filled n lots / refunded.
5. **My orders.** Open orders with filled/remaining, status, Cancel button;
   incoming pending parks; history of closed orders.
6. **Activity.** Feed for §27: order created, partially filled, filled,
   cancelled, expired, trade settled, settlement failed. Built from signals and
   diffing the polls; no separate store.

Design and quality: dark/light aware, integer-safe forms, disabled states with
reasons, no blocking spinners, tabular numerals for prices, accessible colour
contrast, and keyboard-operable order ticket. No trading-terminal chrome: this
is a demo of the protocol.

Tests: Vitest for `formatAmount` / `parseAmount` and the API layer with a
stubbed client; one Playwright script driving two tabs through the §30
scenario against two sandboxes.

**Accept when:** the §30 scenario works by hand between two browser tabs (see
below). **Commit:** UI in its own commits, separate from any zome change.

## Demo script (also the manual acceptance test)

1. Two agents, A (Alice) and B (Bob). Each clicks *Get test funds* (Alice 100 A,
   Bob 200 B; adjust the faucet UI to mint specific amounts).
2. Alice: Sell, 100 A, price 1.20, expiry 1 h → book shows `1.20 · 100`.
3. Bob: clicks the level, takes 40 A → sees "waiting for maker", then "filled 40".
4. Alice's wallet: +48.00 B; locked 60 A. Book shows `1.20 · 60`.
5. Alice cancels → 60 A returns; the level disappears.
6. Repeat with expiry 1 minute and let it lapse; then a race: two takers on
   one order, one filled and one refunded.
7. Notifications for each step appear in the Activity feed.

## Risks to keep visible

- **Cross-zome decode in validation** (Phase 2) is the least certain piece. If
  decoding `Escrow` from the dex integrity zome does not work, fall back to
  putting `MarketToOrders` in `ledger_integrity` and say so in the README; it
  weakens the "book is separate from ledger" story but keeps the guarantees.
- **N+1 reads** on `get_order_book` will slow down with many orders.
- **Maker offline** leaves takers waiting; only mitigated, not solved (README).
- **Unyt swap-out**: keep the dex zome free of ledger internals so replacing
  `ledger` still touches only its own crates and `ledger_api`.
- Every integrity or `dna.yaml` change creates a new network. Flag it in the
  commit message and keep it in its own commit.

## Order of work and commits

1. `Extract ledger_api crate`
2. `Add dex_core::book`
3. `Add dex integrity zome (DNA-hash impacting)`
4. `Add dex coordinator`
5. `Add signals and maker auto-run`
6. `Add Sweettest cases for order book and edge cases`
7. `ui:` scaffold, api layer, wallet, book, ticket, take flow, my orders, activity
8. README update (status table, limitations, how to run two agents)

## Status (2026-09-29)

Done, on branch `order-book`: steps 1–7 above, with one addition before
Phase 3: an upgrade to Holochain 0.7 (`hdk 0.7.0` / `hdi 0.8.0`). Step 8 is
done as part of the market-order docs.

Not done / open:
- A test for a run that omits the oldest park: needs a hook that commits a
  hand-picked run (validation accepts it by design).
- Demo step 6 (1-minute expiry, two-taker race) is covered in Sweettest, not
  in the Playwright script.

## Market orders — done

Taker-only, immediate-or-cancel, slippage-limited sweeps (see README
"Market orders"). No ledger, integrity or `dna.yaml` change; the DNA hash is
unchanged.

1. `Add market order planning to dex_core::book`: `plan_market`,
   `plan_market_by_budget`, `plan_with_limit` on top of `plan_take`.
2. `Add market order externs`: `preview_market_order`, `market_order`,
   `market_order_by_budget`, `retry_market_shortfall` (once, original limit).
3. `Add market order Sweettests`: two-level sweep, slippage cut-off with a
   budget, race + retry within the original limit, market sell.
4. `ui: market orders`: Limit / Market toggle, preview, result panel with a
   one-time "Retry remainder".
5. `Document market orders`.

Open question: the limit formulas round buy up and sell down, which loosens
the limit by up to one minor unit per lot. Flip to buy floor / sell ceil if
the limit must never exceed the stated slippage.

## Future milestones (recorded, not scheduled)

- **"Swap X for Y via HF" routing** (after market orders): two market orders,
  X->HF then HF->Y, each with its own slippage limit. Non-atomic: leg 2 can
  fail or fill partially after leg 1 settled, so the result must say clearly
  what was converted, what is left in HF and what to do next.
- **HF mutual credit, modelled on Unyt's credit mechanism:** negative HF
  balances within credit limits. Not implemented now; balances stay
  non-negative. Depends on the Unyt credit questions in README.

## Unyt template port — done

`unyt/dex_order_escrow` (Rhai) does what one `SettlementRun` does;
`tests/rhai_parity` proves it matches `dex_core::execute_run` under the
published `rave_engine` 0.12.0 (see README "Unyt port"). No zome, integrity
or `dna.yaml` change.

Approved design decisions: `requested_lots` rides in the taker spend's own
payload (no separate request role); ties break by source hash after the
host's timestamp sort; the opening maker spend must equal the initial lock
exactly; roles `maker_spender` and `taker_spender`.

1. `Add dex_order_escrow Rhai template`
2. `Add Rhai parity harness`: 4,000 generated runs identical, every dex_core
   scenario, formatter and parsers, 13,129 operations at the 20-park cap.
3. `Document Unyt template port`

Open: the [DNA] items and questions 1–12 in README "Swapping in real Unyt";
replacing the `ledger` zomes with Unyt itself.

## Milestone 2 — taker protection and validation performance (in progress)

One integrity (DNA-hash) change: (A) park timeout / reclaim, (B) balance
checkpoints; plus client-side (C) maker presence check and (D) faster
Sweettests. Design: `docs/design/taker-protection.md`.

Decision (2026-09-30, asked before design): **multi-market support (generic
pair, per-market lot and tick size) is folded into this same integrity
change**, so the network resets once. Designed in section 11 of the design
doc and approved, with a **hub asset**: units and markets live in DNA
properties, every market is quoted in the hub unit **HF** (markets are
X/HF only), and the default demo market is **A/HF** (UNIT-B renamed HF,
same numbers).

Order of work: (a) `Amounts` as a normalised unit map, alone and
behaviour-preserving (done, `94a95b4`); then Phase 2 steps 1-8.
