# Order-book and trade read performance

**Status: approved and implemented** (milestone 4; commits in PLAN.md).

Milestone 4 design. Pinned: hdk 0.7.0, hdi 0.8.0, holochain
0.7.0; every Holochain claim below was read in that source.

## 1. The problem, measured

Every book and price read walks every order ever listed in its market:

| Read | Calls today |
|---|---|
| `get_order_book`, `plan_take`, `take`, market orders | 1 `get_links` on the market anchor (every listing ever), then for each listing whose tag has not expired: a cross-zome call to `ledger.get_escrow_state` = 1 `get` (escrow) + 1 `get_links` (runs) + 1 `get` per run |
| `get_market_stats`, `get_candles`, `get_recent_trades` | the same `get_links`, then for each listing that could have traded in the window: `ledger.get_escrow_trades` = 1 `get` + 1 `get_links` + 1 `get` per run |

Tag-expired listings are skipped without a read, but **closed orders
(filled, cancelled) are read until their expiry**, and every listing ever made
is returned by the `get_links` and deserialised, forever.

**Baseline** (`tests/sweettest/src/bench.rs`, `#[ignore]`; one maker lists N
orders: 10 live, the rest split evenly between cancelled, filled (one trade
each) and expired; a second agent on another conductor times each read,
median of 3 after a warm-up; local sandbox, both nodes full-arc, so no
network latency is in these numbers):

| Orders listed | Trades | Book | Stats | Candles (24 h) | Recent trades |
|---:|---:|---:|---:|---:|---:|
| 10 | 0 | 38 ms | 74 ms | 37 ms | 37 ms |
| 100 | 30 | 355 ms | 478 ms | 477 ms | 487 ms |
| 1000 | 330 | 7,236 ms | 10,454 ms | 10,562 ms | 10,565 ms |

Cost grows with the market's whole history, and faster than linearly
(10× the orders, ~20× the time at 1000), with the per-order cross-zome
state or trade read dominating, not the `get_links` itself. At 1000 listed
orders, of which only 10 are live, the book takes 7 s and every price read
10 s. On a real network each per-order read can be a network round trip, so
these are lower bounds.

**Found while measuring (writes, out of scope here):** building the
1000-order market took 3,610 s. Every ledger write reads the author's whole
source chain (`own_chain()`, a `query` of every record, for the checkpoint
and balance checks in the coordinator), so a maker's writes slow down as
their history grows: O(n) per write, O(n²) for a history. Validation is
bounded by checkpoints (milestone 2); the coordinator is not. Proposed as a
follow-up: have the coordinator read from the latest checkpoint too (query
the chain from its sequence number), a coordinator-only change.

## 2. What Holochain gives us (verified)

- `LinkQuery` has `before` / `after` (link timestamp) and `author` filters
  (`holochain_zome_types` 0.7.0 `query.rs:81`). The authority applies them
  (`holochain_cascade` 0.7.0 `authority.rs:286-292`), **but after loading
  every live link on the base** (`holochain_state` 0.7.0
  `dht_store/reads.rs:1163`, `get_live_link_actions(base)`, then a Rust
  filter). A time filter shrinks what crosses the network and what the
  reader handles; it does not shrink the authority's work or storage, and a
  single anchor stays a hot spot holding the market's whole history.
- Deleted links are tombstoned and excluded from `get_live_link_actions`.
- A link tag is immutable, arbitrary bytes (<= 1 KB in practice), and
  validation can read anything it can `must_get_*`.

## 3. Proposal

### 3.1 Maximum order lifetime

A DNA property `max_order_lifetime_secs` (production 7 days = 604,800). An
order's `expires_at` minus its escrow's action timestamp must be at most
that. With it, **a listing created more than `max_order_lifetime` ago is
certainly expired**, so readers can ignore everything older.

Where to validate: **on the listing link** (`dex_integrity`), not on the
`Escrow` (`ledger_integrity`) as first sketched. The bound serves the book,
and the ledger is the stand-in for Unyt, whose agreements will not carry a
DEX rule; a longer-lived escrow can still exist in the ledger, it just
cannot be listed. (Either works under the mock; the listing is the right
owner.)

### 3.2 Listings in per-market, per-day buckets

The listing link's base becomes `Path ["listings", market_hex, day]`, where
`day = floor(escrow_action_timestamp / 86,400 s)` (UTC days since the
epoch). Tag and target unchanged. A book read fetches the buckets for
`day(now − max_lifetime) ..= day(now)`: **8 `get_links` for 7 days**, each
bucket holding one day's listings, instead of one anchor holding all of
them.

This bounds the book by the orders listed in the last 7 days. It does not by
itself drop closed orders within those 7 days, so add:

### 3.3 The maker deletes a listing once its order is closed

When a run leaves an order closed (fully filled, cancelled or released
after expiry), the maker's coordinator deletes the listing link in the same
call. Validation allows a `DeleteLink` of a listing only by the link's
author (the maker). Best effort: an offline maker's closed or expired
listings stay until they return; the lifetime bound (3.1) and the tag's
expiry filter still hide expired ones, and a closed-but-undeleted order
costs one state read, as today. With this, a book read costs **8
`get_links` + one state read per open order** (plus stragglers).

### 3.4 A trade index per market per day

After each run that sells lots, the maker's dex coordinator writes a link
from `Path ["trades", market_hex, day(run timestamp)]` to the run, with a
self-describing tag:

```
version u8 | price_per_lot u64 BE | lots u64 BE | maker_side u8 | run_ts i64 BE | escrow ActionHash (39 bytes)
```

Validation makes the tag trustworthy, so readers never read runs:

- the target is a `SettlementRun` of the ledger, authored by the link's
  author;
- its escrow is in the market the base names, and the base's day is the run
  timestamp's UTC day;
- `lots` equals the lots the run sold: `remaining(prev lock) −
  remaining(run.locked)` for a `Fill` run, where the previous lock is the
  `prev_run`'s `locked` or the escrow's initial lock, and must be > 0;
  price, side, run timestamp and escrow equal the escrow's and the run's;
- at most one index link per run is not enforceable (no proof of absence);
  readers deduplicate by target, as they do listings.

Reads: stats (24 h) = 2 `get_links` (today and yesterday), candles = one per
day in range (24 h: 2; 7 d: 8), recent trades = today, then earlier days
until the limit is met (capped at 30 days). **No per-trade or per-order
reads.** `dex_trades` keeps the maths; the trades come from tags instead of
runs. The last price older than 30 days is reported as unknown (a stated
limit).

## 4. Alternatives

| Option | Book cost | Trade reads | Integrity change | Verdict |
|---|---|---|---|---|
| **A. Proposal: lifetime + day buckets + delete on close + trade index** | 8 `get_links` + open orders | 1–8 `get_links`, no per-trade reads | yes | **Recommended** |
| B. Delete on close only (single anchor) | 1 `get_links` over live links + open orders | unchanged (all history) | yes (allow delete) | Fixes the book only when makers are online; expired orders of absent makers pile up; nothing for trades |
| C. Maker-published order summary (one entry per maker listing their open orders, updated each run) | makers-index `get_links` + 1 `get` per maker | unchanged | yes (new entry type, update chains) | Needs a maker index that also grows; stale when a maker is offline; update-chain reads; more validation for little gain over A |
| D. `after` time filter on the existing anchor + lifetime | 1 `get_links` (authority still scans all history) + listings of the last 7 days | `after`-filtered, still per-order runs | lifetime only | Cheapest to build, but the authority's work and storage still grow forever and trades still cost per order |
| E. Hour buckets instead of day buckets | 169 `get_links` for 7 days | 25 for 24 h stats, finer for the 1 h chart | yes | Many more calls for the book and stats; only pays off for trades above ~50,000/day/market |

**Day vs hour.** Day buckets make the common reads cheap in calls (book 8,
stats 2) and keep a bucket's size proportional to a day's activity. A market
with 10,000 trades a day puts 10,000 ~70-byte tags in a bucket (~0.7 MB per
day read): fine at MVP scale. If a market outgrows that, the trade index
alone can move to hour buckets later without touching listings (new link
type, readers try both during a migration window).

## 5. Integrity changes (DNA-hash impacting, own commit)

`dex_core` (links into both integrity zomes, so itself DNA-hash impacting):

- `DexProperties.max_order_lifetime_secs` (required, > 0; genesis and
  validation refuse missing or malformed, as for timing today).
- `buckets::utc_day(ts_us) -> i64`, `buckets::days_between(from, to)`,
  `buckets::live_listing_days(now, max_lifetime)`, trade tag encode/decode,
  and `run_sold_lots(terms, market, prev_locked, run_locked, mode)`: pure,
  tested (boundaries at midnight, negative timestamps, 7-day window).

`ledger_api`: `SettlementRun` moves into its `entries` module (as `Escrow`
did) with a `SETTLEMENT_RUN_ENTRY_INDEX` asserted by `ledger_integrity`, so
`dex_integrity` can decode runs. No change to the run's content.

`dex_integrity`:

- `MarketToOrders` validation: base == `listing_anchor(escrow.market,
  utc_day(escrow action timestamp))`; `escrow.terms.expires_at −
  escrow timestamp <= max_order_lifetime`; tag as today.
- `DeleteLink` of a `MarketToOrders` link: allowed only by the author of
  the deleted `CreateLink` (fetched with `must_get_action`).
- New `MarketTradesByDay` link type with the validation in 3.4.

`ledger_integrity`: only the `ledger_api` move (entry index assertion).
`dna.yaml`: `max_order_lifetime_secs: 604800`.

Coordinator changes (no DNA hash): `place_order`/`list` use the day anchor;
book and trade reads use the buckets; `run_until_settled` writes the trade
index link and deletes the listing when the order closes; the UI read paths
are unchanged in shape (same externs), and price data can refresh with the
book again.

**Migration:** none; a new DNA hash is a new network, as for milestone 2.

## 6. The Rhai template

**No change.** The template computes one run from the agreement's inputs and
parked spends. Nothing here changes a run's inputs, outputs, the park
payload or the settlement maths: listing buckets, the lifetime bound, the
listing delete and the trade index are all DEX-side links around the
ledger. On Unyt, the trade index would point at RAVEs, and its validation
would need to read them, which a DEX integrity zome cannot do across DNAs:
the index would then be maker-attested (unvalidated) or need a Unyt-side
helper; a new Unyt question. The parity harness needs no change and will be
rerun to confirm.

## 7. Plan after approval

1. `dex_core`: properties field, bucket maths, trade tag, `run_sold_lots`
   (DNA-hash impacting; unit tests).
2. Integrity: `ledger_api` run move, `dex_integrity` listing bucket and
   lifetime, listing delete, trade index validation; crafted tests
   (DNA-hash impacting).
3. Coordinator: bucketed book and trade reads, trade index writes, delete on
   close; Sweettests (bucket boundaries with an injected day, deletes by
   others refused, forged trade tags refused).
4. UI read paths (price refresh cadence back to the book's; no shape change).
5. Rerun the benchmark; before/after table; docs.

Full test suite after each commit; stop on any failure.

## 8. Result

Benchmark after implementation (same setup; see PLAN.md for commits):

| Orders listed | Trades | Book | Stats | Candles (24 h) | Recent trades |
|---:|---:|---:|---:|---:|---:|
| 10 | 0 | 47 ms | 33 ms | 4 ms | 32 ms |
| 100 | 30 | 53 ms | 5 ms | 5 ms | 4 ms |
| 1000 | 330 | 196 ms | 23 ms | 24 ms | 23 ms |

Price reads are flat. The book grows mildly: the benchmark lists all 1000
orders on one day, and its 330 expired orders are never released by an
absent maker, so never unlisted; the book fetches their links and skips them
by tag. Bounded by the 7-day lifetime.

Write cost was not improved: the 1000-order build took 3,818 s (3,610 s
before). Per phase at 1000 orders of history: place 267 ms, cancel 4.8 s,
settle 5.7 s each (at 100: 155 ms, 630 ms, 814 ms). The benchmark settles
every order after all are placed, so a run's validation walk back to its
escrow covers nearly the whole history; the coordinator also decodes every
run the maker has and scans the day's listing bucket. Split and fix in the
follow-up.

The Rhai template is unchanged; the parity harness passes (31/31, 4,000
generated runs identical).
