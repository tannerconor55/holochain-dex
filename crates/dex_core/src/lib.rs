//! # dex_core
//!
//! Pure, deterministic settlement logic for the UNIT-A / HF limit-order DEX.
//!
//! This crate has no Holochain dependency. The same function, [`execute_run`], is
//! called by the maker's coordinator to *produce* a settlement run and by every
//! validating peer to *re-derive* it — exactly how a Unyt RAVE is re-run by peers.
//! Keeping it pure means it can be unit-tested natively and later ported line for
//! line into the Rhai Smart Agreement template.
//!
//! ## Units
//!
//! All money is integer minor units (`u64`). No floats anywhere.
//!
//! * Both assets use 2 decimals: `100` minor units = `1.00`.
//! * A *lot* is the market's `lot_size` minor units of its base unit (the
//!   demo market A/HF: 100, so 1.00 A).
//! * An order's price is `price_per_lot`: HF minor units paid per lot.
//!   `1.20 HF per A` with a 1.00 A lot is `price_per_lot = 120`.
//! * Quote for a fill = `lots × price_per_lot`, an exact integer.

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

pub use markets::{MarketDef, MarketId};

pub mod book;
pub mod buckets;
pub mod checkpoint;
pub mod listing;
pub mod markets;
pub mod properties;
pub mod timeout;

/// Maximum parked taker requests one settlement run may consume.
///
/// Bounds the work in a single run so that spam requests cannot make every run
/// (including the maker's release) too expensive to execute or validate. Parks
/// beyond the cap simply wait for the next run.
pub const MAX_PARKS_PER_RUN: usize = 20;

/// Largest amount of either asset a single test-faucet mint may create.
pub const MAX_MINT: u64 = 1_000_000 * 100;

// ---------------------------------------------------------------------------
// Amounts
// ---------------------------------------------------------------------------

/// A unit's id, as the DNA properties declare it (`"A"`, `"HF"`, ...).
pub type UnitId = String;

/// UNIT-A, the default market's base unit.
pub const UNIT_A: &str = "A";

/// HF, the hub unit every market is quoted in.
pub const HUB_UNIT: &str = "HF";

/// Minor units by unit. The MVP analogue of a Unyt unit map.
///
/// Normalised: a zero-valued unit is never stored, so equal amounts are always
/// equal maps and always serialise to the same bytes. An ordered map, so
/// iteration and serialisation are deterministic. Deserialising a map that
/// holds a zero or an empty unit id is refused rather than normalised, so a
/// non-canonical form can never be read back as an entry.
#[derive(Debug, Default, Clone, PartialEq, Eq, Hash, Serialize)]
#[serde(transparent)]
pub struct Amounts(BTreeMap<UnitId, u64>);

impl<'de> Deserialize<'de> for Amounts {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let map = BTreeMap::<UnitId, u64>::deserialize(deserializer)?;
        if map.values().any(|v| *v == 0) {
            return Err(serde::de::Error::custom("amounts must not hold a zero-valued unit"));
        }
        if map.keys().any(|k| k.is_empty()) {
            return Err(serde::de::Error::custom("amounts must not hold an empty unit id"));
        }
        Ok(Amounts(map))
    }
}

impl Amounts {
    pub const ZERO: Amounts = Amounts(BTreeMap::new());

    /// The default market's pair: `a` minor units of A and `hf` of HF.
    pub fn new(a: u64, hf: u64) -> Self {
        let mut out = Self::ZERO;
        out.put(UNIT_A, a);
        out.put(HUB_UNIT, hf);
        out
    }

    pub fn of(unit: &str, amount: u64) -> Self {
        let mut out = Self::ZERO;
        out.put(unit, amount);
        out
    }

    fn put(&mut self, unit: &str, amount: u64) {
        if amount == 0 {
            self.0.remove(unit);
        } else {
            self.0.insert(unit.to_string(), amount);
        }
    }

    /// The amount of `unit`; absent means zero.
    pub fn get(&self, unit: &str) -> u64 {
        self.0.get(unit).copied().unwrap_or(0)
    }

    /// Every non-zero unit, in unit-id order.
    pub fn units(&self) -> impl Iterator<Item = (&str, u64)> {
        self.0.iter().map(|(unit, amount)| (unit.as_str(), *amount))
    }

    pub fn is_zero(&self) -> bool {
        self.0.is_empty()
    }

    pub fn checked_add(&self, other: &Amounts) -> Option<Amounts> {
        let mut out = self.clone();
        for (unit, amount) in other.units() {
            let sum = out.get(unit).checked_add(amount)?;
            out.put(unit, sum);
        }
        Some(out)
    }

    /// `None` if any unit would go below zero.
    pub fn checked_sub(&self, other: &Amounts) -> Option<Amounts> {
        let mut out = self.clone();
        for (unit, amount) in other.units() {
            let difference = out.get(unit).checked_sub(amount)?;
            out.put(unit, difference);
        }
        Some(out)
    }

    /// True if every unit of `self` is at least the matching unit of `other`.
    pub fn covers(&self, other: &Amounts) -> bool {
        other.units().all(|(unit, amount)| self.get(unit) >= amount)
    }
}

// ---------------------------------------------------------------------------
// Orders
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Side {
    /// Maker sells UNIT-A for HF. Maker escrows A.
    Sell,
    /// Maker buys UNIT-A with HF. Maker escrows HF.
    Buy,
}

/// The fixed terms of one order. The MVP analogue of a Smart Agreement's
/// `Fixed` inputs: set once when the escrow opens, never changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrderTerms {
    pub side: Side,
    /// HF minor units per lot.
    pub price_per_lot: u64,
    /// Lots offered (sell) or wanted (buy).
    pub lots: u64,
    /// Expiry, microseconds since the Unix epoch (Holochain `Timestamp` scale).
    pub expires_at: i64,
}

/// Order maths in a market: the market gives the units, the lot size and the
/// tick size; the terms give the side, price and quantity.
impl OrderTerms {
    /// The unit the maker escrows and delivers: base for a sell, quote for a buy.
    pub fn maker_asset<'m>(&self, market: &'m MarketDef) -> &'m str {
        match self.side {
            Side::Sell => &market.base,
            Side::Buy => &market.quote,
        }
    }

    /// The unit takers pay with.
    pub fn taker_asset<'m>(&self, market: &'m MarketDef) -> &'m str {
        match self.side {
            Side::Sell => &market.quote,
            Side::Buy => &market.base,
        }
    }

    /// Maker-asset minor units delivered per lot filled.
    pub fn maker_units_per_lot(&self, market: &MarketDef) -> u64 {
        match self.side {
            Side::Sell => market.lot_size,
            Side::Buy => self.price_per_lot,
        }
    }

    /// Taker-asset minor units paid per lot filled.
    pub fn taker_units_per_lot(&self, market: &MarketDef) -> u64 {
        match self.side {
            Side::Sell => self.price_per_lot,
            Side::Buy => market.lot_size,
        }
    }

    pub fn validate(&self, market: &MarketDef) -> Result<(), CoreError> {
        if self.lots == 0 {
            return Err(CoreError::InvalidTerms("lots must be positive"));
        }
        if self.price_per_lot == 0 {
            return Err(CoreError::InvalidTerms("price_per_lot must be positive"));
        }
        if market.lot_size == 0 || market.tick_size == 0 {
            return Err(CoreError::InvalidTerms("the market's lot and tick sizes must be positive"));
        }
        #[allow(clippy::manual_is_multiple_of)] // keep MSRV-friendly for older holonix toolchains
        if self.price_per_lot % market.tick_size != 0 {
            return Err(CoreError::InvalidTerms("price_per_lot must be a multiple of the tick size"));
        }
        self.initial_lock(market)?;
        Ok(())
    }

    /// What the maker must lock when opening the escrow.
    pub fn initial_lock(&self, market: &MarketDef) -> Result<Amounts, CoreError> {
        let amount = self
            .lots
            .checked_mul(self.maker_units_per_lot(market))
            .ok_or(CoreError::Overflow)?;
        Ok(Amounts::of(self.maker_asset(market), amount))
    }

    /// Lots still fillable, derived from a locked amount. The lock is the only
    /// source of truth for the remaining quantity.
    pub fn remaining_lots(&self, locked: &Amounts, market: &MarketDef) -> Result<u64, CoreError> {
        let maker_asset = self.maker_asset(market);
        if locked.units().any(|(unit, _)| unit != maker_asset) {
            return Err(CoreError::InconsistentLock);
        }
        let held = locked.get(maker_asset);
        let per_lot = self.maker_units_per_lot(market);
        #[allow(clippy::manual_is_multiple_of)] // keep MSRV-friendly for older holonix toolchains
        if held % per_lot != 0 {
            return Err(CoreError::InconsistentLock);
        }
        Ok(held / per_lot)
    }

    pub fn is_expired_at(&self, now: i64) -> bool {
        now >= self.expires_at
    }
}

// ---------------------------------------------------------------------------
// Settlement runs
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RunMode {
    /// Fill parked takers in time order; keep the remainder locked.
    Fill,
    /// Refund every consumed taker and return the whole remaining lock to the
    /// maker. Covers both cancellation and post-expiry release.
    Release,
}

/// A taker's parked request, as seen by a settlement run.
///
/// `P` is the park's identifier (an `ActionHash` in the zome), `K` the taker's
/// identity (an `AgentPubKey`). Generic so this crate stays Holochain-free.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParkInput<P, K> {
    pub id: P,
    pub taker: K,
    /// Funds the taker parked. Anything not used by a fill is refunded.
    pub amounts: Amounts,
    pub requested_lots: u64,
    /// When the park was authored (µs). Establishes time priority.
    pub parked_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunInput<P, K> {
    pub terms: OrderTerms,
    /// The order's market: its units, lot size and tick size.
    pub market: MarketDef,
    pub maker: K,
    /// Lock carried from the previous run (or the escrow's initial lock).
    pub prev_locked: Amounts,
    /// Parks this run consumes, in any order. At most [`MAX_PARKS_PER_RUN`].
    pub parks: Vec<ParkInput<P, K>>,
    /// The run's timestamp (µs). Maker-asserted, like Unyt's `executed_timestamp`.
    pub now: i64,
    pub mode: RunMode,
}

/// A payment produced by a run. One per receiver per run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Allocation<K> {
    pub receiver: K,
    pub amounts: Amounts,
}

/// How one consumed park was settled. Recorded for the UI and notifications.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParkOutcome<P> {
    pub id: P,
    pub filled_lots: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunOutput<P, K> {
    pub allocations: Vec<Allocation<K>>,
    pub locked: Amounts,
    /// Parks in the order they were processed (time priority).
    pub consumed: Vec<P>,
    pub outcomes: Vec<ParkOutcome<P>>,
    pub filled_lots: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CoreError {
    InvalidTerms(&'static str),
    TooManyParks,
    DuplicatePark,
    InconsistentLock,
    Overflow,
    ConservationViolated,
}

impl core::fmt::Display for CoreError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            CoreError::InvalidTerms(why) => write!(f, "invalid order terms: {why}"),
            CoreError::TooManyParks => {
                write!(f, "a run may consume at most {MAX_PARKS_PER_RUN} parks")
            }
            CoreError::DuplicatePark => write!(f, "a park appears twice in one run"),
            CoreError::InconsistentLock => {
                write!(f, "lock does not correspond to a whole number of lots")
            }
            CoreError::Overflow => write!(f, "arithmetic overflow"),
            CoreError::ConservationViolated => write!(f, "run does not conserve funds"),
        }
    }
}

impl std::error::Error for CoreError {}

/// Execute one settlement run. Deterministic: the same input always produces
/// the same output, independent of the order of `input.parks`.
///
/// For each park, in `(parked_at, id)` order:
/// * `fill = min(requested_lots, remaining_lots, lots the taker's payment covers)`,
///   or `0` when releasing or when the order has expired;
/// * the taker receives `fill` lots of the maker asset plus a refund of every
///   parked unit not spent on the fill;
/// * the maker receives the taker-asset cost of the fill.
///
/// `Fill` keeps the unfilled remainder locked; `Release` pays it back to the maker.
pub fn execute_run<P, K>(input: &RunInput<P, K>) -> Result<RunOutput<P, K>, CoreError>
where
    P: Clone + Ord,
    K: Clone + PartialEq,
{
    let terms = &input.terms;
    let market = &input.market;
    terms.validate(market)?;

    if input.parks.len() > MAX_PARKS_PER_RUN {
        return Err(CoreError::TooManyParks);
    }
    let unique: BTreeSet<&P> = input.parks.iter().map(|p| &p.id).collect();
    if unique.len() != input.parks.len() {
        return Err(CoreError::DuplicatePark);
    }

    let mut parks: Vec<&ParkInput<P, K>> = input.parks.iter().collect();
    parks.sort_by(|x, y| (x.parked_at, &x.id).cmp(&(y.parked_at, &y.id)));

    let fillable = input.mode == RunMode::Fill && !terms.is_expired_at(input.now);
    let maker_asset = terms.maker_asset(market);
    let taker_asset = terms.taker_asset(market);
    let maker_per_lot = terms.maker_units_per_lot(market);
    let taker_per_lot = terms.taker_units_per_lot(market);

    let mut remaining = terms.remaining_lots(&input.prev_locked, market)?;
    let mut allocations: Vec<Allocation<K>> = Vec::new();
    let mut consumed = Vec::with_capacity(parks.len());
    let mut outcomes = Vec::with_capacity(parks.len());
    let mut filled_total: u64 = 0;

    for park in parks {
        let affordable = park.amounts.get(taker_asset) / taker_per_lot;
        let fill = if fillable {
            park.requested_lots.min(remaining).min(affordable)
        } else {
            0
        };
        remaining -= fill;
        filled_total = filled_total.checked_add(fill).ok_or(CoreError::Overflow)?;

        let taker_pays = Amounts::of(
            taker_asset,
            fill.checked_mul(taker_per_lot).ok_or(CoreError::Overflow)?,
        );
        let taker_gets = Amounts::of(
            maker_asset,
            fill.checked_mul(maker_per_lot).ok_or(CoreError::Overflow)?,
        );
        // `affordable` guarantees the park covers `taker_pays`.
        let refund = park
            .amounts
            .checked_sub(&taker_pays)
            .ok_or(CoreError::Overflow)?;

        credit(
            &mut allocations,
            &park.taker,
            taker_gets.checked_add(&refund).ok_or(CoreError::Overflow)?,
        )?;
        credit(&mut allocations, &input.maker, taker_pays)?;

        consumed.push(park.id.clone());
        outcomes.push(ParkOutcome {
            id: park.id.clone(),
            filled_lots: fill,
        });
    }

    let remaining_lock = Amounts::of(
        maker_asset,
        remaining
            .checked_mul(maker_per_lot)
            .ok_or(CoreError::Overflow)?,
    );
    let locked = match input.mode {
        RunMode::Fill => remaining_lock,
        RunMode::Release => {
            credit(&mut allocations, &input.maker, remaining_lock)?;
            Amounts::ZERO
        }
    };

    let output = RunOutput {
        allocations,
        locked,
        consumed,
        outcomes,
        filled_lots: filled_total,
    };
    check_conservation(input, &output)?;
    Ok(output)
}

/// Add `amounts` to `receiver`'s allocation, creating it on first use.
/// Zero amounts create nothing, so no receiver is ever sent an empty payment.
fn credit<K: Clone + PartialEq>(
    allocations: &mut Vec<Allocation<K>>,
    receiver: &K,
    amounts: Amounts,
) -> Result<(), CoreError> {
    if amounts.is_zero() {
        return Ok(());
    }
    match allocations.iter_mut().find(|a| &a.receiver == receiver) {
        Some(existing) => {
            existing.amounts = existing
                .amounts
                .checked_add(&amounts)
                .ok_or(CoreError::Overflow)?;
        }
        None => allocations.push(Allocation {
            receiver: receiver.clone(),
            amounts,
        }),
    }
    Ok(())
}

/// Per asset: `paid out + newly locked == parked inputs + previously locked`.
/// The same rule Unyt's DNA is documented to enforce on every RAVE.
pub fn check_conservation<P, K>(
    input: &RunInput<P, K>,
    output: &RunOutput<P, K>,
) -> Result<(), CoreError> {
    let mut inflow = input.prev_locked.clone();
    for park in &input.parks {
        inflow = inflow.checked_add(&park.amounts).ok_or(CoreError::Overflow)?;
    }
    let mut outflow = output.locked.clone();
    for alloc in &output.allocations {
        outflow = outflow
            .checked_add(&alloc.amounts)
            .ok_or(CoreError::Overflow)?;
    }
    if inflow == outflow {
        Ok(())
    } else {
        Err(CoreError::ConservationViolated)
    }
}

/// Select which pending parks the next run should consume: time priority,
/// capped at [`MAX_PARKS_PER_RUN`]. The rest wait for a later run.
pub fn select_parks<P, K>(mut pending: Vec<ParkInput<P, K>>) -> Vec<ParkInput<P, K>>
where
    P: Ord,
{
    pending.sort_by(|x, y| (x.parked_at, &x.id).cmp(&(y.parked_at, &y.id)));
    pending.truncate(MAX_PARKS_PER_RUN);
    pending
}

#[cfg(test)]
mod tests;
