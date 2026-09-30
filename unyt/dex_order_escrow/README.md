# dex_order_escrow

One limit order's escrow on the UNIT-A / UNIT-B DEX, as a Unyt Smart Agreement.
One agreement instance per order (`ea_id` is the order id). Each execution is
one settlement run: it fills parked taker spends in time priority at the
order's price, refunds what it cannot fill, and carries the unfilled
remainder as `locked` to the maker's next run. In Release mode (cancel, or
after expiry) it refunds every consumed taker and returns the whole lock to
the maker.

It is a port of `dex_core::execute_run` from the dex-mvp repository and is
checked against it case by case by `tests/rhai_parity` in that repository.

Enforcement tags: **[engine]** is behaviour of the published `rave_engine`
crate (0.12.0 read for this template); **[DNA]** is enforced by the Unyt DNA,
which is not public; **[convention]** is a library or UI convention.

## Roles

| Role id | Parked link | Who | Why this id |
|---|---|---|---|
| `maker_spender` | `ParkedSpendBalance` | the order's maker, `Authorized: [maker]` | Parks the opening lock. Contains `spender`, so the app offers it as a spend-parking send action [convention], which is what it is. |
| `taker_spender` | `ParkedSpendBalance` | anyone, `Any` | Parks the taker's payment in the taker asset, with `{ "requested_lots": n, "park_deadline": micros }` in the spend's payload. Contains `spender` for the same reason. Neither id contains a collect substring (`receiver`, `payee`, `depositor`): both parties are paid through `unyt_allocation`, not by a collecting role. |

There is no separate request role. `requested_lots` and `park_deadline`
travel in the taker spend's own payload and reach the script as inputs of the
same names, one entry per spend with the same `link_hash`. One park is one link, so a request
cannot be separated from, or paired with the wrong, spend. That the DNA fills
a `ProvidedBy` input named `requested_lots` from the spend link's payload is
[DNA]: the crate publishes the lookup (`ParkedLink::get_input_for_key`: any
input name other than `<role>_allocations`, `amount`, `new_balance`,
`fees_owed` or `global_definition` is read from the link's payload) but not
its caller.

**Executor:** `AuthorizedExecutor` = the maker. Required: the lock is carried
through `previous_execution`, which is derived from the executor's own chain
[engine: `derive_previous_execution`], so a second executor would start from
no lock; the DNA is also documented to refuse a lock under `Any` [DNA].

## Inputs

| Input | Branch | Instruction | Type |
|---|---|---|---|
| `maker_spender_allocations` | `consumed_inputs` | `ProvidedBy: maker_spender` | array of `{ amount, source }` |
| `taker_spender_allocations` | `consumed_inputs` | `ProvidedBy: taker_spender` | array of `{ amount, source }` |
| `requested_lots` | `consumed_inputs` | `ProvidedBy: taker_spender` | array of integers (from each spend's payload) |
| `park_deadline` | `consumed_inputs` | `ProvidedBy: taker_spender` | array of integers, microseconds since the Unix epoch (from each spend's payload) |
| `previous_execution` | `inputs` | `Custom: GetPreviousExecution` | `{ id, output }` or null |
| `side` | `inputs` | `Fixed` | `"Sell"` (maker escrows UNIT-A) or `"Buy"` (maker escrows UNIT-B) |
| `price_per_lot` | `inputs` | `Fixed` | integer, UNIT-B minor units per lot (120 = 1.20 B per A) |
| `lots` | `inputs` | `Fixed` | integer, lots offered or wanted |
| `expires_at` | `inputs` | `Fixed` | integer, microseconds since the Unix epoch |
| `lot_size` | `inputs` | `Fixed` | integer, UNIT-A minor units per lot (100 = 1.00 A) |
| `unit_a`, `unit_b` | `inputs` | `Fixed` | unit index strings of UNIT-A and UNIT-B |
| `max_parks_per_run` | `inputs` | `Fixed` | integer, 20 |
| `mode` | `inputs` | `ExecutorProvided` | `"Fill"` or `"Release"` |

The preset `executed_timestamp` is the only clock [engine]; the script parses
its RFC3339 form (0, 3 or 6 fractional digits, `Z`) to microseconds with
integer date arithmetic, since no helper does.

**`mode` is the maker's choice**, checked for shape only [engine:
`validate_input_rules`], and that is safe: Release can only return the maker's
own lock to the maker and refund every consumed taker in full; it can never
pay the maker a taker's funds. Fill can only fill at the order's own price.
The maker already controls when (and whether) runs happen, so choosing the
mode gives them nothing new.

## What a run does

Integer minor units throughout (both assets have 2 decimals); amount strings
are parsed on the way in and formatted by one formatter on the way out.

1. **The starting lock.** The opening run (no `previous_execution`) requires
   exactly one maker spend holding exactly `lots x per-lot` of the maker
   asset and nothing else, names it in a name-only allocation to the maker,
   and starts from it. Later runs start from `previous_execution.output.locked`;
   a maker spend parked after the opening run is refunded in full.
2. **Priority.** Taker spends are sorted by source hash, then by park
   timestamp with the host's stable `acceding_sort_allocation` [engine], so
   the order is (timestamp, source hash). A spend that does not resolve to a
   parked link fails the run rather than vanishing.
3. **Deadline.** A spend whose declared `park_deadline` is at or before
   `executed_timestamp`, or that declares none, goes in `rejected_links` and
   is never consumed [engine: rejected links stay on chain], matching the
   mock ledger's rule that a run may consume a park only before its deadline
   (`dex_core::timeout::run_may_consume`). The deadline is
   `min(parked_at + park timeout, expires_at) + settle grace`, as
   `dex_core::timeout::park_deadline` computes it.
4. **Cap.** Of the spends still in time, the first `max_parks_per_run` are
   consumed; the rest go in `rejected_links` and wait for the next run.
   Expired spends do not count against the cap.
5. **Fill.** For each consumed spend: `fill = min(requested_lots, remaining,
   paid / taker_per_lot)`, or 0 in Release mode or at/after `expires_at`. The
   taker receives `fill` lots of the maker asset (sources: the spend and the
   lock) plus a refund of every unit of the spend not spent on the fill; the
   maker receives the fill's cost. A spend with no usable `requested_lots` is
   refunded in full. Units outside the pair are refunded as their exact
   strings.
6. **Remainder.** Fill: `locked = { maker_unit: remaining }`, or `{}` when
   nothing remains. Release: the whole remainder is paid to the maker and
   `locked = {}`.

Every consumed spend is named by an allocation and refunded if not filled
(consumed inputs are spent whatever the output says [DNA]). Per unit:
`paid + locked == parked sources + previous lock`.

## Output

```json
{
  "unyt_allocation": [
    { "receiver": "<taker>", "amounts": { "1": "40.00" }, "sources": ["<lock>", "<spend>"] },
    { "receiver": "<maker>", "amounts": { "2": "48.00" }, "sources": ["<spend>"] }
  ],
  "locked": { "1": "60.00" },
  "computed_values": {
    "mode": "Fill", "consumed": ["<spend>"],
    "outcomes": [{ "park": "<spend>", "filled_lots": 40 }],
    "filled_lots": 40, "remaining_lots": 60, "deferred_parks": 0, "expired_parks": 0
  }
}
```

One allocation per receiver, holding both units when both apply (a unit map
may hold several indexes [engine]). No monetary amount is ever `"0"`; an
allocation that only names sources has `amounts: {}` [engine: name-only].
`rejected_links` lists spends past their deadline (`expired_parks`) and
beyond the cap (`deferred_parks`), in time order, each with its reason.

## Trust assumptions

- **The maker runs the order** and chooses when and in which mode. A maker who
  never runs leaves takers' spends parked; one still in time is refunded by
  the next run, and one past its deadline stays parked, never consumable.
  **Getting it back is [DNA]:** the mock ledger lets the taker reclaim it
  (design doc `docs/design/taker-protection.md`, section 4.2), but whether a
  parked spend can be withdrawn by its author on Unyt is an open question
  (dex-mvp README, Unyt question 1). A template cannot write a reclaim.
- **`park_deadline` is the taker's own claim** about their own spend, since
  no registered helper returns a parked link's timestamp [engine]. An
  earlier deadline only makes their own spend unconsumable sooner; a later
  one only keeps it consumable (fillable, at the order's price) for longer.
  Neither can take anyone else's funds. A helper returning the link
  timestamp would let the template compute it instead (dex-mvp README, Unyt
  questions).
- **Taker priority across the run is the host's timestamp order** [engine];
  which spends a run is handed is [DNA]. Validation re-runs the script, but
  cannot force a maker to run at all.
- **`requested_lots` is the taker's own claim about their own spend.** A false
  one can only hurt that taker: the fill is capped by what their spend pays for
  and by the lots remaining.
- **Unit precision.** Amounts are written with exactly 2 decimals, so both
  units must allow at least 2 [engine: `normalize_precision`]; the unit
  definitions are network configuration.

## Known gaps

- Conservation per unit, and that a source counts only once an allocation
  names it, are [DNA]; the parity harness checks the first itself.
- Where the DNA fills `requested_lots` and `park_deadline` from the spend
  payload is [DNA] (see Roles). If it does not, the fallback is a separate `ParkedData(true)` request
  role naming its spend, paired by `get_spend_links_author`.
- Ties within one microsecond break by source-hash string. The dex-mvp mock
  ledger breaks them by hash bytes; once this template replaces it, this order
  is the rule.
- A carried lock after a DNA migration cannot yet be named as a source
  (`GetCarryForward` has no `id`) [engine]; this template uses
  `GetPreviousExecution` and does not handle migration.
