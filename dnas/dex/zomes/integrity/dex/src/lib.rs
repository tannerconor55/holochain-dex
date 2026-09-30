//! # dex_integrity — order book listings
//!
//! An order is listed by a `MarketToOrders` link from its market's anchor to
//! the order's ledger escrow. Markets come from the DNA properties. There are no entry types: the escrow already holds
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
//! * **Its own market.** The base is the anchor of the escrow's market, and
//!   the tag starts with that market's id.
//! * **Listings are permanent.** Link deletes are rejected. Filled, cancelled
//!   and expired orders leave the book because readers filter on ledger state.
//!
//! The same escrow may be listed more than once (a retried listing); readers
//! deduplicate by target.

use dex_core::listing::encode_tag;
use dex_core::properties::DexProperties;
use dex_core::MarketId;
use hdi::prelude::*;
use ledger_api::{Escrow, ESCROW_ENTRY_INDEX, LEDGER_INTEGRITY_ZOME};

/// The hash a market's listing links hang off: one anchor per market, so a
/// book read fetches one market's links only. A plain path hash: listings are
/// found by `get_links` on it, so no path links (`ensure`) are needed.
pub fn market_anchor(market: &MarketId) -> ExternResult<EntryHash> {
    Path::from(vec![Component::from("market"), Component::from(market.to_hex())]).path_entry_hash()
}

/// The DNA properties, checked; `Err` says why they are unusable.
pub fn load_properties() -> ExternResult<Result<DexProperties, String>> {
    let bytes = dna_info()?.modifiers.properties;
    let props: DexProperties = match holochain_serialized_bytes::decode(bytes.bytes()) {
        Ok(props) => props,
        Err(e) => return Ok(Err(format!("malformed DNA properties: {e}"))),
    };
    Ok(props.check().map(|()| props).map_err(|e| format!("invalid DNA properties: {e}")))
}

/// An agent cannot join a DNA whose properties are missing or malformed.
#[hdk_extern]
pub fn genesis_self_check(_data: GenesisSelfCheckData) -> ExternResult<ValidateCallbackResult> {
    match load_properties()? {
        Ok(_) => valid(),
        Err(why) => invalid(why),
    }
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
    let props = match load_properties()? {
        Ok(props) => props,
        Err(why) => return invalid(why),
    };
    ensure!(props.market(&escrow.market).is_ok(), "the escrow's market is not declared");
    ensure!(
        link.base_address == AnyLinkableHash::from(market_anchor(&escrow.market)?),
        "a listing must hang off its escrow's market anchor"
    );
    ensure!(
        link.tag.0 == encode_tag(&escrow.market, &escrow.terms),
        "listing tag does not match the escrow's market and terms"
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
