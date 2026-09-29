//! # dex_integrity — order book listings
//!
//! An order is listed by a `MarketToOrders` link from the market anchor to the
//! order's ledger escrow. There are no entry types: the escrow already holds
//! the terms, and a second copy could disagree with it.
//!
//! ## What validation guarantees
//!
//! * **A listing is an escrow.** The target must be a valid `Escrow` entry of
//!   the `ledger_integrity` zome.
//! * **Only the maker lists.** The link's author is the escrow's author.
//! * **Listings cannot lie.** The tag must equal
//!   [`dex_core::listing::encode_tag`] of the escrow's terms, so side, price
//!   and expiry in the book are exactly the escrow's.
//! * **One market.** The base is the anchor for [`MARKET`].
//! * **Listings are permanent.** Link deletes are rejected. Filled, cancelled
//!   and expired orders leave the book because readers filter on ledger state.
//!
//! The same escrow may be listed more than once (a retried listing); readers
//! deduplicate by target.

use dex_core::listing::encode_tag;
use hdi::prelude::*;
use ledger_api::{Escrow, ESCROW_ENTRY_INDEX, LEDGER_INTEGRITY_ZOME};

/// The single market this DNA lists.
pub const MARKET: &str = "unit_a_unit_b";

/// The hash every listing link hangs off. A plain path hash: listings are
/// found by `get_links` on it, so no path links (`ensure`) are needed.
pub fn market_anchor() -> ExternResult<EntryHash> {
    Path::from(MARKET).path_entry_hash()
}

#[hdk_link_types]
pub enum LinkTypes {
    /// Market anchor → an order's escrow. Tag: `dex_core::listing` layout.
    MarketToOrders,
}

fn valid() -> ExternResult<ValidateCallbackResult> {
    Ok(ValidateCallbackResult::Valid)
}

fn invalid(reason: impl Into<String>) -> ExternResult<ValidateCallbackResult> {
    Ok(ValidateCallbackResult::Invalid(reason.into()))
}

macro_rules! ensure {
    ($cond:expr, $($msg:tt)+) => {
        if !$cond {
            return invalid(format!($($msg)+));
        }
    };
}

#[hdk_extern]
pub fn validate(op: Op) -> ExternResult<ValidateCallbackResult> {
    match op.flattened::<(), LinkTypes>()? {
        FlatOp::CreateRecord(OpRecord::CreateLink { link_type, action })
        | FlatOp::Link(OpLink::CreateLink { link_type, action }) => match link_type {
            LinkTypes::MarketToOrders => validate_listing(&action),
        },
        FlatOp::CreateRecord(OpRecord::DeleteLink { .. }) | FlatOp::Link(OpLink::DeleteLink { .. }) => {
            invalid("order book listings are permanent")
        }
        _ => valid(),
    }
}

fn validate_listing(action: &TypedAction<CreateLinkData>) -> ExternResult<ValidateCallbackResult> {
    let link = &action.data;
    ensure!(
        link.base_address == AnyLinkableHash::from(market_anchor()?),
        "listings must hang off the {MARKET} market anchor"
    );
    let Some(escrow_hash) = link.target_address.clone().into_action_hash() else {
        return invalid("a listing must target an escrow action");
    };
    let record = must_get_valid_record(escrow_hash)?;
    ensure!(is_ledger_escrow(record.action())?, "a listing must target a ledger escrow");
    ensure!(
        record.action().author() == action.author(),
        "only the escrow's maker may list it"
    );
    // A valid Create of a public entry always carries its entry.
    let entry = record
        .entry()
        .as_option()
        .ok_or_else(|| wasm_error!(WasmErrorInner::Guest("escrow record has no entry".into())))?;
    let escrow = Escrow::try_from(entry)?;
    ensure!(
        link.tag.0 == encode_tag(&escrow.terms),
        "listing tag does not match the escrow's terms"
    );
    valid()
}

/// A `Create` of `ledger_integrity`'s `Escrow` entry type.
fn is_ledger_escrow(action: &Action) -> ExternResult<bool> {
    let ActionData::Create(create) = &action.data else {
        return Ok(false);
    };
    let EntryType::App(def) = &create.entry_type else {
        return Ok(false);
    };
    // `zome_names` is in zome-index order, integrity zomes first.
    let ledger = ZomeName::from(LEDGER_INTEGRITY_ZOME);
    let Some(ledger_index) = dna_info()?.zome_names.iter().position(|z| z == &ledger) else {
        return Err(wasm_error!(WasmErrorInner::Guest(format!(
            "{LEDGER_INTEGRITY_ZOME} is not in this DNA"
        ))));
    };
    Ok(usize::from(def.zome_index.0) == ledger_index && def.entry_index.0 == ESCROW_ENTRY_INDEX)
}
