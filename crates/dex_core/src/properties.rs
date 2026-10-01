//! # DNA properties
//!
//! Everything the DNA's validation depends on that is not code: the park
//! timeout, the units and the markets. Parsed from the DNA properties by the
//! integrity zomes' `genesis_self_check` (an agent cannot join a DNA whose
//! properties fail [`DexProperties::check`]) and by `validate` (which never
//! falls back to a default). One parser, one set of rules, both callers.

use crate::markets::{MarketDef, MarketId};
use crate::{Amounts, UnitId, HUB_UNIT};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

/// Longest unit id the properties may declare.
pub const MAX_UNIT_ID_LEN: usize = 16;

/// Most decimals a unit may declare (u64 minor units still hold 18 digits).
pub const MAX_DECIMALS: u8 = 18;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UnitDef {
    pub id: UnitId,
    /// Minor units per whole unit = 10^decimals.
    pub decimals: u8,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DexProperties {
    /// A park may be consumed until this long after it was parked (or the
    /// order's expiry, if sooner), plus the grace.
    pub park_timeout_secs: u64,
    pub settle_grace_secs: u64,
    /// The longest an order may be listed for: `expires_at` minus the
    /// escrow's creation time. Bounds which listing days a book read needs.
    pub max_order_lifetime_secs: u64,
    pub units: Vec<UnitDef>,
    pub markets: Vec<MarketDef>,
}

/// The two durations in microseconds, as validation compares timestamps.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Timing {
    pub park_timeout_us: i64,
    pub settle_grace_us: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PropertiesError {
    ZeroParkTimeout,
    ZeroOrderLifetime,
    DurationOverflow,
    NoUnits,
    BadUnitId(String),
    DuplicateUnit(String),
    TooManyDecimals(String),
    NoHubUnit,
    NoMarkets,
    BaseIsQuote(String),
    NotQuotedInHub { base: String, quote: String },
    UndefinedUnit(String),
    ZeroLotSize(MarketId),
    ZeroTickSize(MarketId),
    DuplicateMarket(MarketId),
    InverseMarketListed(MarketId),
    UnknownMarket(MarketId),
}

impl core::fmt::Display for PropertiesError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        use PropertiesError::*;
        match self {
            ZeroParkTimeout => write!(f, "park_timeout_secs must be positive"),
            ZeroOrderLifetime => write!(f, "max_order_lifetime_secs must be positive"),
            DurationOverflow => write!(f, "a duration does not fit in microseconds"),
            NoUnits => write!(f, "no units declared"),
            BadUnitId(id) => write!(f, "unit id {id:?} must be 1-{MAX_UNIT_ID_LEN} ASCII letters, digits or '_'"),
            DuplicateUnit(id) => write!(f, "unit {id} declared twice"),
            TooManyDecimals(id) => write!(f, "unit {id} declares more than {MAX_DECIMALS} decimals"),
            NoHubUnit => write!(f, "the hub unit {HUB_UNIT} is not declared"),
            NoMarkets => write!(f, "no markets declared"),
            BaseIsQuote(id) => write!(f, "market {id}/{id} trades a unit against itself"),
            NotQuotedInHub { base, quote } => {
                write!(f, "market {base}/{quote} is not quoted in the hub unit {HUB_UNIT}")
            }
            UndefinedUnit(id) => write!(f, "unit {id} is not declared"),
            ZeroLotSize(id) => write!(f, "market {id} has a zero lot size"),
            ZeroTickSize(id) => write!(f, "market {id} has a zero tick size"),
            DuplicateMarket(id) => write!(f, "market {id} declared twice"),
            InverseMarketListed(id) => write!(f, "market {id} is listed together with its inverse"),
            UnknownMarket(id) => write!(f, "market {id} is not declared"),
        }
    }
}

impl std::error::Error for PropertiesError {}

fn valid_unit_id(id: &str) -> bool {
    (1..=MAX_UNIT_ID_LEN).contains(&id.len()) && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

fn secs_to_micros(secs: u64) -> Result<i64, PropertiesError> {
    i64::try_from(secs)
        .ok()
        .and_then(|s| s.checked_mul(1_000_000))
        .ok_or(PropertiesError::DurationOverflow)
}

impl DexProperties {
    /// Every rule a DNA's properties must meet. `genesis_self_check` refuses an
    /// agent on a DNA that fails it; `validate` rechecks what it relies on.
    pub fn check(&self) -> Result<(), PropertiesError> {
        if self.park_timeout_secs == 0 {
            return Err(PropertiesError::ZeroParkTimeout);
        }
        self.timing()?;
        if self.max_order_lifetime_secs == 0 {
            return Err(PropertiesError::ZeroOrderLifetime);
        }
        self.max_order_lifetime_us()?;
        if self.units.is_empty() {
            return Err(PropertiesError::NoUnits);
        }
        let mut units = BTreeSet::new();
        for u in &self.units {
            if !valid_unit_id(&u.id) {
                return Err(PropertiesError::BadUnitId(u.id.clone()));
            }
            if u.decimals > MAX_DECIMALS {
                return Err(PropertiesError::TooManyDecimals(u.id.clone()));
            }
            if !units.insert(u.id.as_str()) {
                return Err(PropertiesError::DuplicateUnit(u.id.clone()));
            }
        }
        if !units.contains(HUB_UNIT) {
            return Err(PropertiesError::NoHubUnit);
        }
        if self.markets.is_empty() {
            return Err(PropertiesError::NoMarkets);
        }
        let ids: Vec<MarketId> = self.markets.iter().map(MarketDef::id).collect();
        let mut seen = BTreeSet::new();
        for (m, id) in self.markets.iter().zip(&ids) {
            if m.base == m.quote {
                return Err(PropertiesError::BaseIsQuote(m.base.clone()));
            }
            for unit in [&m.base, &m.quote] {
                if !units.contains(unit.as_str()) {
                    return Err(PropertiesError::UndefinedUnit(unit.clone()));
                }
            }
            if m.quote != HUB_UNIT {
                return Err(PropertiesError::NotQuotedInHub { base: m.base.clone(), quote: m.quote.clone() });
            }
            if m.lot_size == 0 {
                return Err(PropertiesError::ZeroLotSize(*id));
            }
            if m.tick_size == 0 {
                return Err(PropertiesError::ZeroTickSize(*id));
            }
            if !seen.insert(*id) {
                return Err(PropertiesError::DuplicateMarket(*id));
            }
        }
        // Redundant with the hub rule (an inverse of X/HF is HF/X, not quoted
        // in HF), and checked anyway so it holds if that rule ever relaxes.
        for m in &self.markets {
            if ids.contains(&MarketId::of(&m.quote, &m.base)) {
                return Err(PropertiesError::InverseMarketListed(m.id()));
            }
        }
        Ok(())
    }

    pub fn timing(&self) -> Result<Timing, PropertiesError> {
        Ok(Timing {
            park_timeout_us: secs_to_micros(self.park_timeout_secs)?,
            settle_grace_us: secs_to_micros(self.settle_grace_secs)?,
        })
    }

    pub fn max_order_lifetime_us(&self) -> Result<i64, PropertiesError> {
        secs_to_micros(self.max_order_lifetime_secs)
    }

    pub fn market(&self, id: &MarketId) -> Result<&MarketDef, PropertiesError> {
        self.markets
            .iter()
            .find(|m| &m.id() == id)
            .ok_or(PropertiesError::UnknownMarket(*id))
    }

    pub fn unit(&self, id: &str) -> Option<&UnitDef> {
        self.units.iter().find(|u| u.id == id)
    }

    /// Refuse amounts holding a unit the properties do not declare.
    pub fn check_amounts(&self, amounts: &Amounts) -> Result<(), PropertiesError> {
        match amounts.units().find(|(unit, _)| self.unit(unit).is_none()) {
            Some((unit, _)) => Err(PropertiesError::UndefinedUnit(unit.to_string())),
            None => Ok(()),
        }
    }

    /// Production values: 30 min timeout, 5 min grace, units A and HF with two
    /// decimals, and the demo market A/HF.
    pub fn demo() -> Self {
        DexProperties {
            park_timeout_secs: 30 * 60,
            settle_grace_secs: 5 * 60,
            max_order_lifetime_secs: 7 * 86_400,
            units: vec![
                UnitDef { id: crate::UNIT_A.into(), decimals: 2 },
                UnitDef { id: HUB_UNIT.into(), decimals: 2 },
            ],
            markets: vec![MarketDef::default_pair()],
        }
    }
}

#[cfg(test)]
mod tests;
