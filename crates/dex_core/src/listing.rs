//! # Listing tags
//!
//! An order is listed in the book by a link from the market anchor to its
//! escrow. The link tag carries the fields the book filters and sorts on, so a
//! reader can prefilter without fetching every escrow. Integrity validation
//! recomputes the tag from the escrow's terms, so a tag can never disagree
//! with the order it lists.
//!
//! Layout (17 bytes): `side (1) | price_per_lot (u64 BE) | expires_at (i64 BE)`.
//! Big-endian so that, within one side, byte order is price order.
//!
//! [`encode_tag`] and [`decode_tag`] are the only encoder and decoder, used by
//! both the integrity and coordinator zomes.

use crate::{OrderTerms, Side};

pub const TAG_LEN: usize = 17;

const SELL: u8 = 0;
const BUY: u8 = 1;

/// The fields a listing tag carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ListingTag {
    pub side: Side,
    pub price_per_lot: u64,
    pub expires_at: i64,
}

impl From<&OrderTerms> for ListingTag {
    fn from(terms: &OrderTerms) -> Self {
        Self {
            side: terms.side,
            price_per_lot: terms.price_per_lot,
            expires_at: terms.expires_at,
        }
    }
}

pub fn side_byte(side: Side) -> u8 {
    match side {
        Side::Sell => SELL,
        Side::Buy => BUY,
    }
}

pub fn encode_tag(terms: &OrderTerms) -> Vec<u8> {
    let mut tag = Vec::with_capacity(TAG_LEN);
    tag.push(side_byte(terms.side));
    tag.extend_from_slice(&terms.price_per_lot.to_be_bytes());
    tag.extend_from_slice(&terms.expires_at.to_be_bytes());
    tag
}

/// `None` if the bytes are not a well-formed listing tag.
pub fn decode_tag(bytes: &[u8]) -> Option<ListingTag> {
    if bytes.len() != TAG_LEN {
        return None;
    }
    let side = match bytes[0] {
        SELL => Side::Sell,
        BUY => Side::Buy,
        _ => return None,
    };
    let price_per_lot = u64::from_be_bytes(bytes[1..9].try_into().ok()?);
    let expires_at = i64::from_be_bytes(bytes[9..17].try_into().ok()?);
    Some(ListingTag {
        side,
        price_per_lot,
        expires_at,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn terms(side: Side, price: u64, expires_at: i64) -> OrderTerms {
        OrderTerms {
            side,
            price_per_lot: price,
            lots: 100,
            expires_at,
        }
    }

    #[test]
    fn round_trips_both_sides() {
        for side in [Side::Sell, Side::Buy] {
            let t = terms(side, 120, 1_700_000_000_000_000);
            let tag = encode_tag(&t);
            assert_eq!(tag.len(), TAG_LEN);
            assert_eq!(decode_tag(&tag), Some(ListingTag::from(&t)));
        }
    }

    #[test]
    fn lots_are_not_part_of_the_tag() {
        // Remaining quantity comes from the ledger only.
        let mut other = terms(Side::Sell, 120, 5);
        other.lots = 7;
        assert_eq!(encode_tag(&terms(Side::Sell, 120, 5)), encode_tag(&other));
    }

    #[test]
    fn rejects_malformed_tags() {
        let good = encode_tag(&terms(Side::Buy, 1, 1));
        assert_eq!(decode_tag(&good[..16]), None);
        assert_eq!(decode_tag(&[good.clone(), vec![0]].concat()), None);
        let mut bad_side = good;
        bad_side[0] = 2;
        assert_eq!(decode_tag(&bad_side), None);
        assert_eq!(decode_tag(&[]), None);
    }

    #[test]
    fn byte_order_is_price_order_within_a_side() {
        let prices = [0, 1, 119, 120, 255, 256, 65_536, u64::MAX];
        for pair in prices.windows(2) {
            let low = encode_tag(&terms(Side::Sell, pair[0], i64::MAX));
            let high = encode_tag(&terms(Side::Sell, pair[1], 0));
            assert!(low < high, "{} should sort before {}", pair[0], pair[1]);
        }
        // Sides never interleave.
        let best_bid = encode_tag(&terms(Side::Buy, 0, 0));
        let worst_ask = encode_tag(&terms(Side::Sell, u64::MAX, i64::MAX));
        assert!(worst_ask < best_bid);
    }
}
