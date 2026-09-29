//! # dex_core
//!
//! Pure, deterministic settlement logic for the UNIT-A / UNIT-B limit-order DEX.
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
//! * A *lot* is [`LOT_SIZE_A`] minor units of UNIT-A (1.00 A).
//! * An order's price is `price_per_lot`: UNIT-B minor units paid per lot.
//!   `1.20 B per A` with a 1.00 A lot is `price_per_lot = 120`.
//! * Quote for a fill = `lots × price_per_lot`, an exact integer.

use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

/// Minor units of UNIT-A in one tradable lot (1.00 A).
pub const LOT_SIZE_A: u64 = 100;

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

/// Amounts of both assets, in minor units. The MVP analogue of a Unyt unit map.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Amounts {
    pub a: u64,
    pub b: u64,
}

impl Amounts {
    pub const ZERO: Amounts = Amounts { a: 0, b: 0 };

    pub fn new(a: u64, b: u64) -> Self {
        Self { a, b }
    }

    pub fn of(asset: Asset, amount: u64) -> Self {
        match asset {
            Asset::A => Self { a: amount, b: 0 },
            Asset::B => Self { a: 0, b: amount },
        }
    }

    pub fn get(&self, asset: Asset) -> u64 {
        match asset {
            Asset::A => self.a,
            Asset::B => self.b,
        }
    }

    pub fn is_zero(&self) -> bool {
        self.a == 0 && self.b == 0
    }

    pub fn checked_add(self, other: Amounts) -> Option<Amounts> {
        Some(Amounts {
            a: self.a.checked_add(other.a)?,
            b: self.b.checked_add(other.b)?,
        })
    }

    pub fn checked_sub(self, other: Amounts) -> Option<Amounts> {
        Some(Amounts {
            a: self.a.checked_sub(other.a)?,
            b: self.b.checked_sub(other.b)?,
        })
    }

    /// True if every component of `self` is at least the matching component of `other`.
    pub fn covers(&self, other: &Amounts) -> bool {
        self.a >= other.a && self.b >= other.b
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Asset {
    A,
    B,
}

// ---------------------------------------------------------------------------
// Orders
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Side {
    /// Maker sells UNIT-A for UNIT-B. Maker escrows A.
    Sell,
    /// Maker buys UNIT-A with UNIT-B. Maker escrows B.
    Buy,
}

/// The fixed terms of one order. The MVP analogue of a Smart Agreement's
/// `Fixed` inputs: set once when the escrow opens, never changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrderTerms {
    pub side: Side,
    /// UNIT-B minor units per lot.
    pub price_per_lot: u64,
    /// Lots offered (sell) or wanted (buy).
    pub lots: u64,
    /// Expiry, microseconds since the Unix epoch (Holochain `Timestamp` scale).
    pub expires_at: i64,
}

impl OrderTerms {
    /// The asset the maker escrows and delivers.
    pub fn maker_asset(&self) -> Asset {
        match self.side {
            Side::Sell => Asset::A,
            Side::Buy => Asset::B,
        }
    }

    /// The asset takers pay with.
    pub fn taker_asset(&self) -> Asset {
        match self.side {
            Side::Sell => Asset::B,
            Side::Buy => Asset::A,
        }
    }

    /// Maker-asset minor units delivered per lot filled.
    pub fn maker_units_per_lot(&self) -> u64 {
        match self.side {
            Side::Sell => LOT_SIZE_A,
            Side::Buy => self.price_per_lot,
        }
    }

    /// Taker-asset minor units paid per lot filled.
    pub fn taker_units_per_lot(&self) -> u64 {
        match self.side {
            Side::Sell => self.price_per_lot,
            Side::Buy => LOT_SIZE_A,
        }
    }

    pub fn validate(&self) -> Result<(), CoreError> {
        if self.lots == 0 {
            return Err(CoreError::InvalidTerms("lots must be positive"));
        }
        if self.price_per_lot == 0 {
            return Err(CoreError::InvalidTerms("price_per_lot must be positive"));
        }
        self.initial_lock()?;
        Ok(())
    }

    /// What the maker must lock when opening the escrow.
    pub fn initial_lock(&self) -> Result<Amounts, CoreError> {
        let amount = self
            .lots
            .checked_mul(self.maker_units_per_lot())
            .ok_or(CoreError::Overflow)?;
        Ok(Amounts::of(self.maker_asset(), amount))
    }

    /// Lots still fillable, derived from a locked amount. The lock is the only
    /// source of truth for the remaining quantity.
    pub fn remaining_lots(&self, locked: &Amounts) -> Result<u64, CoreError> {
        if locked.get(self.taker_asset()) != 0 {
            return Err(CoreError::InconsistentLock);
        }
        let held = locked.get(self.maker_asset());
        let per_lot = self.maker_units_per_lot();
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
    terms.validate()?;

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
    let maker_asset = terms.maker_asset();
    let taker_asset = terms.taker_asset();
    let maker_per_lot = terms.maker_units_per_lot();
    let taker_per_lot = terms.taker_units_per_lot();

    let mut remaining = terms.remaining_lots(&input.prev_locked)?;
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
            .checked_sub(taker_pays)
            .ok_or(CoreError::Overflow)?;

        credit(
            &mut allocations,
            &park.taker,
            taker_gets.checked_add(refund).ok_or(CoreError::Overflow)?,
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
                .checked_add(amounts)
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
    let mut inflow = input.prev_locked;
    for park in &input.parks {
        inflow = inflow.checked_add(park.amounts).ok_or(CoreError::Overflow)?;
    }
    let mut outflow = output.locked;
    for alloc in &output.allocations {
        outflow = outflow
            .checked_add(alloc.amounts)
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
