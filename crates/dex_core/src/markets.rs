//! # Markets
//!
//! A market is a pair of units with a lot size and a tick size. Markets are
//! declared in the DNA properties (see [`crate::properties`]); every market is
//! quoted in the hub unit [`crate::HUB_UNIT`].
//!
//! A market's id is derived from its pair alone, so one pair can never be
//! declared twice with different lot or tick sizes.

use crate::UnitId;
use blake2::digest::{consts::U32, Digest};
use serde::{Deserialize, Serialize};

type Blake2b256 = blake2::Blake2b<U32>;

/// Domain separation for market ids, so no other hash in the system collides.
const MARKET_ID_DOMAIN: &[u8] = b"dex-market-id-v1";

/// blake2b-256 of the pair. Serialised as 64 lowercase hex characters.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct MarketId(pub [u8; 32]);

impl MarketId {
    /// The id of the pair `base`/`quote`. Unit ids hold no NUL byte (the
    /// properties check refuses one), so the encoding is unambiguous.
    pub fn of(base: &str, quote: &str) -> Self {
        let mut hasher = Blake2b256::new();
        hasher.update(MARKET_ID_DOMAIN);
        hasher.update([0u8]);
        hasher.update(base.as_bytes());
        hasher.update([0u8]);
        hasher.update(quote.as_bytes());
        MarketId(hasher.finalize().into())
    }

    pub fn to_hex(&self) -> String {
        self.0.iter().map(|b| format!("{b:02x}")).collect()
    }

    pub fn from_hex(s: &str) -> Option<Self> {
        if s.len() != 64 {
            return None;
        }
        let mut out = [0u8; 32];
        for (i, byte) in out.iter_mut().enumerate() {
            *byte = u8::from_str_radix(s.get(2 * i..2 * i + 2)?, 16).ok()?;
        }
        // Only the canonical (lowercase) form, so one id has one spelling.
        (Self(out).to_hex() == s).then_some(Self(out))
    }
}

impl core::fmt::Debug for MarketId {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "MarketId({})", self.to_hex())
    }
}

impl core::fmt::Display for MarketId {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(&self.to_hex())
    }
}

impl Serialize for MarketId {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_hex())
    }
}

impl<'de> Deserialize<'de> for MarketId {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        MarketId::from_hex(&s).ok_or_else(|| serde::de::Error::custom("market id must be 64 lowercase hex characters"))
    }
}

/// One market as the DNA properties declare it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MarketDef {
    /// The unit traded, in lots.
    pub base: UnitId,
    /// The unit prices are in: always the hub unit.
    pub quote: UnitId,
    /// Base minor units in one lot.
    pub lot_size: u64,
    /// Quote minor units; a price per lot must be a positive multiple of it.
    pub tick_size: u64,
}

impl MarketDef {
    pub fn id(&self) -> MarketId {
        MarketId::of(&self.base, &self.quote)
    }

    /// The demo market: A/HF, 1.00 A per lot, prices in steps of 0.01 HF.
    pub fn default_pair() -> Self {
        MarketDef {
            base: crate::UNIT_A.to_string(),
            quote: crate::HUB_UNIT.to_string(),
            lot_size: 100,
            tick_size: 1,
        }
    }
}
