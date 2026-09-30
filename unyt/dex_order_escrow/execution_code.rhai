/// Subject: dex_order_escrow — one limit order's escrow on the UNIT-A / UNIT-B
/// DEX. Each execution is one settlement run: it fills parked taker spends in
/// time priority at the order's price, refunds whatever it cannot fill, and
/// carries the unfilled remainder as `locked` to the next run; or, in Release
/// mode, refunds every consumed taker and returns the whole lock to the maker.
/// A line-for-line port of `dex_core::execute_run` (dex-mvp repository).
///
/// Inputs:
/// maker_spender_allocations: the maker's opening spend (first run only),
///   `{ amount, source }` per parked link; must equal the order's initial lock.
/// taker_spender_allocations: taker spends parked against this order,
///   `{ amount, source }` per parked link, paid in the taker asset.
/// requested_lots: lots each taker wants, read from the payload of the same
///   taker spend link (paired by `link_hash`).
/// park_deadline: the park's deadline, microseconds since the Unix epoch,
///   declared by the taker in the same payload: min(parked_at + park timeout,
///   expires_at) + settle grace (dex_core::timeout::park_deadline). A run may
///   consume a park only before it.
/// previous_execution: `{ id, output }` of this executor's last run of this
///   agreement (Custom GetPreviousExecution); its `output.locked` is the
///   carried lock and its `id` the source a lock-funded allocation names.
/// side: "Sell" (maker escrows UNIT-A) or "Buy" (maker escrows UNIT-B). Fixed.
/// price_per_lot: UNIT-B minor units per lot. Fixed.
/// lots: lots offered (Sell) or wanted (Buy). Fixed.
/// expires_at: expiry, microseconds since the Unix epoch. Fixed.
/// lot_size: UNIT-A minor units in one lot (100 = 1.00 A). Fixed.
/// unit_a / unit_b: unit indexes of UNIT-A and UNIT-B, as strings. Fixed.
/// max_parks_per_run: taker spends one run may consume (20). Fixed.
/// mode: "Fill" or "Release", chosen by the maker (ExecutorProvided).
///
/// Summary:
/// All arithmetic is on integer minor units (both assets have 2 decimals).
/// Amount strings are parsed to integers on the way in and formatted back
/// with one deterministic formatter on the way out; no floats, no fuel
/// helpers on the money path. For each consumed taker spend, in
/// (park timestamp, source hash) order:
///   fill = min(requested_lots, remaining_lots, lots the payment covers),
///          or 0 when releasing or past expiry;
/// the taker receives `fill` lots of the maker asset plus a refund of every
/// unit not spent on the fill, and the maker receives the fill's cost.
/// A spend at or past its declared deadline, or with none, is returned in
/// `rejected_links` and never consumed: it stays parked for its taker.
/// Spends beyond max_parks_per_run are returned in `rejected_links` and wait
/// for the next run. Per unit: paid + locked == parked sources + previous lock.

// ---------------------------------------------------------------------------
// Helpers (the parity harness runs these on their own, above the marker)
// ---------------------------------------------------------------------------

// Both assets have 2 decimals: 100 minor units = 1.00. Functions cannot read
// global constants, so 2 and 100 are written where they are used.

fn all_digits(s) {
    if s.len() == 0 { return false; }
    for c in s.chars() {
        if c < '0' || c > '9' { return false; }
    }
    true
}

/// "123.45" / "123" / "123.4" / "123.450000" -> 12345 minor units. Refuses a
/// negative, a malformed string, or precision finer than 2 decimals.
fn to_minor(s) {
    if type_of(s) != "string" { throw "amount is not a string: " + s; }
    let parts = s.split(".");
    if parts.len() > 2 || !all_digits(parts[0]) { throw "malformed amount: " + s; }
    let whole = parse_int(parts[0]);
    let frac = "";
    if parts.len() == 2 {
        frac = parts[1];
        if !all_digits(frac) { throw "malformed amount: " + s; }
    }
    if frac.len() > 2 {
        for c in frac.sub_string(2, frac.len() - 2).chars() {
            if c != '0' { throw "amount finer than 2 decimals: " + s; }
        }
        frac = frac.sub_string(0, 2);
    }
    while frac.len() < 2 { frac += "0"; }
    whole * 100 + parse_int(frac)
}

/// 12345 -> "123.45", 5 -> "0.05". The one formatter: never "-0", never a
/// float, always exactly two decimals.
fn fmt_minor(n) {
    if type_of(n) != "i64" || n < 0 { throw "cannot format amount: " + n; }
    let frac = n % 100;
    let pad = if frac < 10 { "0" } else { "" };
    (n / 100).to_string() + "." + pad + frac.to_string()
}

/// RFC3339 in UTC as Holochain's Timestamp prints it (0, 3 or 6 fractional
/// digits, "Z") -> microseconds since the Unix epoch. Integer days-from-civil.
fn timestamp_micros(s) {
    if type_of(s) != "string" || s.len() < 20 || s[4] != '-' || s[7] != '-' || s[10] != 'T'
        || s[13] != ':' || s[16] != ':' || s[s.len() - 1] != 'Z' {
        throw "unsupported timestamp: " + s;
    }
    let fields = [s.sub_string(0, 4), s.sub_string(5, 2), s.sub_string(8, 2),
                  s.sub_string(11, 2), s.sub_string(14, 2), s.sub_string(17, 2)];
    for f in fields {
        if !all_digits(f) { throw "unsupported timestamp: " + s; }
    }
    let y = parse_int(fields[0]);
    let m = parse_int(fields[1]);
    let d = parse_int(fields[2]);
    let micros = 0;
    let rest = s.sub_string(19, s.len() - 20); // between the seconds and the "Z"
    if rest.len() > 0 {
        if rest[0] != '.' { throw "unsupported timestamp: " + s; }
        let frac = rest.sub_string(1, rest.len() - 1);
        if !all_digits(frac) || frac.len() > 9 { throw "unsupported timestamp: " + s; }
        if frac.len() > 6 { frac = frac.sub_string(0, 6); }
        while frac.len() < 6 { frac += "0"; }
        micros = parse_int(frac);
    }
    let yy = if m <= 2 { y - 1 } else { y };
    let era = if yy >= 0 { yy / 400 } else { (yy - 399) / 400 };
    let yoe = yy - era * 400;
    let mp = if m > 2 { m - 3 } else { m + 9 };
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146097 + doe - 719468;
    let secs = ((days * 24 + parse_int(fields[3])) * 60 + parse_int(fields[4])) * 60
        + parse_int(fields[5]);
    secs * 1000000 + micros
}

/// The amount of `unit` in a unit map, in minor units; absent -> 0.
fn unit_minor(amounts, unit) {
    let v = amounts[unit];
    if type_of(v) == "()" { 0 } else { to_minor(v) }
}

/// Credit `receiver` with `amount` minor units of `unit` (or an exact
/// amount string for a unit outside the order's pair), and name `source`.
/// Called on the allocation book as `book.credit(...)` so it edits it in
/// place. A zero amount only names the source: a name-only allocation.
fn credit(receiver, unit, amount, source) {
    if !this.receivers.contains(receiver) {
        this.receivers.push(receiver);
        this.by[receiver] = #{ "ints": #{}, "strs": #{}, "sources": [] };
    }
    let a = this.by[receiver];
    if type_of(amount) == "string" {
        a.strs[unit] = if type_of(a.strs[unit]) == "()" { amount } else { add_fuel(a.strs[unit], amount) };
    } else if amount > 0 {
        a.ints[unit] = (a.ints[unit] ?? 0) + amount;
    }
    if type_of(source) != "()" && !a.sources.contains(source) { a.sources.push(source); }
    this.by[receiver] = a;
}

/// Order a spend array by source hash, so the stable timestamp sort that
/// follows breaks ties by hash.
fn by_source(a, b) {
    if a.data.source < b.data.source { -1 } else if a.data.source > b.data.source { 1 } else { 0 }
}

// ---- end of helpers ----

// ---------------------------------------------------------------------------
// Terms
// ---------------------------------------------------------------------------

let side = inputs.side.data;
let price = inputs.price_per_lot.data;
let lots = inputs.lots.data;
let expires_at = inputs.expires_at.data;
let lot_size = inputs.lot_size.data;
let unit_a = inputs.unit_a.data;
let unit_b = inputs.unit_b.data;
let cap = inputs.max_parks_per_run.data;
let mode = inputs.mode.data;

for n in [price, lots, expires_at, lot_size, cap] {
    if type_of(n) != "i64" { throw "order terms must be integers"; }
}
if lots <= 0 { throw "invalid order terms: lots must be positive"; }
if price <= 0 { throw "invalid order terms: price_per_lot must be positive"; }
if lot_size <= 0 || cap <= 0 { throw "invalid order terms: lot_size and max_parks_per_run must be positive"; }
if type_of(unit_a) != "string" || type_of(unit_b) != "string" || unit_a == unit_b {
    throw "unit_a and unit_b must be two different unit index strings";
}
if mode != "Fill" && mode != "Release" { throw "mode must be Fill or Release"; }

let maker_unit = ();
let taker_unit = ();
let maker_per_lot = 0;
let taker_per_lot = 0;
if side == "Sell" {
    maker_unit = unit_a; taker_unit = unit_b; maker_per_lot = lot_size; taker_per_lot = price;
} else if side == "Buy" {
    maker_unit = unit_b; taker_unit = unit_a; maker_per_lot = price; taker_per_lot = lot_size;
} else {
    throw "side must be Sell or Buy";
}
let initial_lock = lots * maker_per_lot; // rhai integer overflow throws
let now = timestamp_micros(executed_timestamp);
let maker = executor_pub_key; // AuthorizedExecutor is the maker

let book = #{ "receivers": [], "by": #{} };

// ---------------------------------------------------------------------------
// The lock this run starts from
// ---------------------------------------------------------------------------

let maker_spends = consumed_inputs.maker_spender_allocations ?? [];
let prev = inputs.previous_execution.data;
let has_prev = type_of(prev) == "map";
let prev_locked = 0;
let lock_source = ();

if has_prev {
    let locked = prev.output.locked ?? #{};
    if unit_minor(locked, taker_unit) != 0 { throw "lock does not correspond to a whole number of lots"; }
    prev_locked = unit_minor(locked, maker_unit);
    lock_source = prev.id;
    // A maker spend after the opening run is refunded in full to its author.
    if maker_spends.len() > 0 {
        for s in acceding_sort_allocation(maker_spends.map(|e| e.data)) {
            for unit in s.amount.keys() {
                if unit == unit_a || unit == unit_b {
                    book.credit(s.spender, unit, to_minor(s.amount[unit]), s.source);
                } else {
                    book.credit(s.spender, unit, s.amount[unit], s.source);
                }
            }
        }
    }
} else {
    // The opening run: exactly one maker spend of exactly the initial lock.
    if maker_spends.len() != 1 { throw "the opening run needs exactly one maker spend"; }
    let opening = maker_spends[0].data;
    for unit in opening.amount.keys() {
        if unit != maker_unit && to_minor(opening.amount[unit]) != 0 {
            throw "the opening spend may only hold the maker asset";
        }
    }
    if unit_minor(opening.amount, maker_unit) != initial_lock {
        throw "the opening spend must equal the initial lock of " + fmt_minor(initial_lock);
    }
    prev_locked = initial_lock;
    lock_source = opening.source;
    book.credit(maker, maker_unit, 0, opening.source); // name it; the maker keeps it locked
}

if prev_locked % maker_per_lot != 0 { throw "lock does not correspond to a whole number of lots"; }
let remaining = prev_locked / maker_per_lot;

// ---------------------------------------------------------------------------
// Taker spends: time priority, capped
// ---------------------------------------------------------------------------

let taker_spends = consumed_inputs.taker_spender_allocations ?? [];
let requested = #{};
for r in consumed_inputs.requested_lots ?? [] {
    requested[r.link_hash] = r.data;
}
let deadlines = #{};
for d in consumed_inputs.park_deadline ?? [] {
    deadlines[d.link_hash] = d.data;
}
let seen = [];
for e in taker_spends {
    if seen.contains(e.data.source) { throw "a park appears twice in one run"; }
    seen.push(e.data.source);
}
let presorted = taker_spends;
presorted.sort(|a, b| by_source(a, b));
let ordered = acceding_sort_allocation(presorted.map(|e| e.data));
if ordered.len() != taker_spends.len() { throw "a taker spend did not resolve to a parked link"; }

// In time order: a park past its deadline is refused outright (the run may
// not consume it: dex_core::timeout::run_may_consume); the rest fill the cap.
let rejected_links = [];
let expired_parks = 0;
let parks = [];
for p in ordered {
    let deadline = deadlines[p.source];
    if type_of(deadline) != "i64" {
        expired_parks += 1;
        rejected_links.push(#{ "hash": p.source, "reason": "no park_deadline in the spend payload; not consumable" });
    } else if now >= deadline {
        expired_parks += 1;
        rejected_links.push(#{ "hash": p.source, "reason": "past its park_deadline; stays parked for its taker" });
    } else if parks.len() < cap {
        parks.push(p);
    } else {
        rejected_links.push(#{ "hash": p.source, "reason": "beyond max_parks_per_run; waits for the next run" });
    }
}

// ---------------------------------------------------------------------------
// The run
// ---------------------------------------------------------------------------

let fillable = mode == "Fill" && now < expires_at;
let consumed = [];
let outcomes = [];
let filled_total = 0;

for p in parks {
    let want = requested[p.source];
    if type_of(want) != "i64" || want < 0 { want = 0; } // no usable request: refund
    let paid = unit_minor(p.amount, taker_unit);
    let affordable = paid / taker_per_lot;
    let fill = 0;
    if fillable {
        fill = want;
        if remaining < fill { fill = remaining; }
        if affordable < fill { fill = affordable; }
    }
    remaining -= fill;
    filled_total += fill;
    let taker_pays = fill * taker_per_lot;
    let taker_gets = fill * maker_per_lot;

    // The taker: the lots filled, plus every parked unit not spent.
    if fill > 0 {
        book.credit(p.spender, maker_unit, taker_gets, lock_source);
    }
    for unit in p.amount.keys() {
        if unit == taker_unit {
            book.credit(p.spender, unit, paid - taker_pays, p.source);
        } else if unit == unit_a || unit == unit_b {
            book.credit(p.spender, unit, to_minor(p.amount[unit]), p.source);
        } else {
            book.credit(p.spender, unit, p.amount[unit], p.source);
        }
    }
    book.credit(p.spender, taker_unit, 0, p.source); // always named
    // The maker: the fill's cost.
    if taker_pays > 0 {
        book.credit(maker, taker_unit, taker_pays, p.source);
    }
    consumed.push(p.source);
    outcomes.push(#{ "park": p.source, "filled_lots": fill });
}

let remaining_lock = remaining * maker_per_lot;
let locked = #{};
if mode == "Fill" {
    if remaining_lock > 0 { locked[maker_unit] = fmt_minor(remaining_lock); }
} else if remaining_lock > 0 {
    book.credit(maker, maker_unit, remaining_lock, lock_source);
}

// ---------------------------------------------------------------------------
// Output
// ---------------------------------------------------------------------------

let allocations = [];
for r in book.receivers {
    let a = book.by[r];
    let amounts = #{};
    for unit in a.ints.keys() {
        if a.ints[unit] > 0 { amounts[unit] = fmt_minor(a.ints[unit]); }
    }
    for unit in a.strs.keys() {
        amounts[unit] = a.strs[unit];
    }
    if amounts.len() > 0 || a.sources.len() > 0 {
        allocations.push(#{ "receiver": r, "amounts": amounts, "sources": a.sources });
    }
}

#{
    "output": #{
        "unyt_allocation": allocations,
        "locked": locked,
        "computed_values": #{
            "mode": mode,
            "consumed": consumed,
            "outcomes": outcomes,
            "filled_lots": filled_total,
            "remaining_lots": if mode == "Fill" { remaining } else { 0 },
            "deferred_parks": rejected_links.len() - expired_parks,
            "expired_parks": expired_parks
        }
    },
    "rejected_links": rejected_links
}
