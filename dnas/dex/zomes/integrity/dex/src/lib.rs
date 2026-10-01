//! # dex_integrity — order book listings and the trade index
//!
//! An order is listed by a `MarketToOrders` link from a per-market, per-day
//! listing anchor to the order's ledger escrow; a sold run is indexed by a
//! `MarketTradesByDay` link from a per-market, per-day trade anchor to the
//! run. Markets and the maximum order lifetime come from the DNA properties.
//! There are no entry types: the escrow and the run already hold the data,
//! and a second copy could disagree with it. Design:
//! `docs/design/read-performance.md`.
//!
//! ## What validation guarantees
//!
//! Listings:
//! * **A listing is an escrow**, of `ledger_integrity`, **listed by its
//!   maker**.
//! * **Listings cannot lie.** The tag equals [`dex_core::listing::encode_tag`]
//!   of the escrow's market and terms.
//! * **Its own market and day.** The base is the listing anchor of the
//!   escrow's market and its creation day (UTC), so a book read fetches only
//!   the days that can still hold live orders.
//! * **A bounded life.** The escrow expires at most `max_order_lifetime_secs`
//!   after it was created; a longer-lived escrow can exist but not be listed.
//! * **Only the maker unlists.** A listing may be deleted by its author only:
//!   the maker removes it once the order closes. An optimisation, never
//!   trusted: readers still check every listed order's state.
//!
//! Trade index:
//! * **The tag is the truth.** The target is a `SettlementRun` by the link's
//!   author (the maker); the base is the trade anchor of its escrow's market
//!   and the run's day; the tag's price, side, run timestamp and escrow are
//!   the escrow's and the run's, and its lots are what the run sold
//!   ([`dex_core::buckets::run_sold_lots`], more than zero). Readers use the
//!   tag without reading the run.
//! * **Index links are permanent.** A maker may still skip one; that trade is
//!   then missing from history and stats (informational, never settlement).
//!
//! The same escrow or run may be linked more than once (a retry); readers
//! deduplicate by target.

use dex_core::buckets::{run_sold_lots, utc_day, within_lifetime, TradeTag};
use dex_core::listing::encode_tag;
use dex_core::properties::DexProperties;
use dex_core::MarketId;
use hdi::prelude::*;
use ledger_api::{Escrow, SettlementRun, ESCROW_ENTRY_INDEX, LEDGER_INTEGRITY_ZOME, SETTLEMENT_RUN_ENTRY_INDEX};

/// The anchor a market's listings created on UTC day `day` hang off.
pub fn listing_anchor(market: &MarketId, day: i64) -> ExternResult<EntryHash> {
    day_anchor("listings", market, day)
}

/// The anchor a market's trades on UTC day `day` hang off.
pub fn trade_anchor(market: &MarketId, day: i64) -> ExternResult<EntryHash> {
    day_anchor("trades", market, day)
}

/// A plain path hash: links are found by `get_links` on it, so no path links
/// (`ensure`) are needed.
fn day_anchor(kind: &str, market: &MarketId, day: i64) -> ExternResult<EntryHash> {
    Path::from(vec![Component::from(kind), Component::from(market.to_hex()), Component::from(day.to_string())])
        .path_entry_hash()
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
    /// Listing anchor (market, creation day) → an order's escrow. Tag:
    /// `dex_core::listing` layout.
    MarketToOrders,
    /// Trade anchor (market, run day) → a run that sold lots. Tag:
    /// `dex_core::buckets::TradeTag`.
    MarketTradesByDay,
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

macro_rules! props_or_invalid {
    () => {
        match load_properties()? {
            Ok(props) => props,
            Err(why) => return invalid(why),
        }
    };
}

#[hdk_extern]
pub fn validate(op: Op) -> ExternResult<ValidateCallbackResult> {
    match op.flattened::<(), LinkTypes>()? {
        FlatOp::CreateRecord(OpRecord::CreateLink { link_type, action })
        | FlatOp::Link(OpLink::CreateLink { link_type, action }) => match link_type {
            LinkTypes::MarketToOrders => validate_listing(&action),
            LinkTypes::MarketTradesByDay => validate_trade_index(&action),
        },
        FlatOp::Link(OpLink::DeleteLink { original_action, link_type, action }) => {
            validate_delete(link_type, original_action.author(), action.author())
        }
        FlatOp::CreateRecord(OpRecord::DeleteLink { action }) => {
            let original = must_get_action(action.data.link_add_address.clone())?;
            let ActionData::CreateLink(create) = &original.hashed.content.data else {
                // System validation guarantees a delete names a create.
                return Err(wasm_error!(WasmErrorInner::Guest("a link delete must name a link create".into())));
            };
            match LinkTypes::from_type(create.zome_index, create.link_type)? {
                Some(link_type) => validate_delete(link_type, original.hashed.content.author(), action.author()),
                None => invalid("not a dex link"),
            }
        }
        _ => valid(),
    }
}

fn validate_delete(link_type: LinkTypes, creator: &AgentPubKey, deleter: &AgentPubKey) -> ExternResult<ValidateCallbackResult> {
    match link_type {
        LinkTypes::MarketToOrders => {
            ensure!(creator == deleter, "only the maker who listed an order may unlist it");
            valid()
        }
        LinkTypes::MarketTradesByDay => invalid("trade index links are permanent"),
    }
}

fn validate_listing(action: &TypedAction<CreateLinkData>) -> ExternResult<ValidateCallbackResult> {
    let link = &action.data;
    let Some(escrow_hash) = link.target_address.clone().into_action_hash() else {
        return invalid("a listing must target an escrow action");
    };
    let record = must_get_valid_record(escrow_hash)?;
    let Some(escrow) = ledger_entry::<Escrow>(&record, ESCROW_ENTRY_INDEX)? else {
        return invalid("a listing must target a ledger escrow");
    };
    ensure!(record.action().author() == action.author(), "only the escrow's maker may list it");
    let props = props_or_invalid!();
    ensure!(props.market(&escrow.market).is_ok(), "the escrow's market is not declared");
    let created = record.action().timestamp().as_micros();
    let lifetime = match props.max_order_lifetime_us() {
        Ok(l) => l,
        Err(why) => return invalid(why.to_string()),
    };
    ensure!(
        within_lifetime(created, escrow.terms.expires_at, lifetime),
        "an order may be listed for at most {} s",
        props.max_order_lifetime_secs
    );
    ensure!(
        link.base_address == AnyLinkableHash::from(listing_anchor(&escrow.market, utc_day(created))?),
        "a listing must hang off its market's anchor for the escrow's creation day"
    );
    ensure!(
        link.tag.0 == encode_tag(&escrow.market, &escrow.terms),
        "listing tag does not match the escrow's market and terms"
    );
    valid()
}

fn validate_trade_index(action: &TypedAction<CreateLinkData>) -> ExternResult<ValidateCallbackResult> {
    let link = &action.data;
    let Some(run_hash) = link.target_address.clone().into_action_hash() else {
        return invalid("a trade index link must target a settlement run");
    };
    let run_record = must_get_valid_record(run_hash)?;
    let Some(run) = ledger_entry::<SettlementRun>(&run_record, SETTLEMENT_RUN_ENTRY_INDEX)? else {
        return invalid("a trade index link must target a ledger settlement run");
    };
    ensure!(run_record.action().author() == action.author(), "only the run's maker may index it");
    let escrow_record = must_get_valid_record(run.escrow.clone())?;
    let Some(escrow) = ledger_entry::<Escrow>(&escrow_record, ESCROW_ENTRY_INDEX)? else {
        return invalid("the run's escrow is not a ledger escrow");
    };
    let props = props_or_invalid!();
    let Ok(market) = props.market(&escrow.market) else {
        return invalid("the escrow's market is not declared");
    };
    let run_ts = run_record.action().timestamp().as_micros();
    ensure!(
        link.base_address == AnyLinkableHash::from(trade_anchor(&escrow.market, utc_day(run_ts))?),
        "a trade index link must hang off its market's anchor for the run's day"
    );
    let Some(tag) = TradeTag::decode(&link.tag.0) else {
        return invalid("malformed trade index tag");
    };
    // The lock before this run: the previous run's, or the escrow's initial lock.
    let prev_locked = match &run.prev_run {
        Some(prev) => match ledger_entry::<SettlementRun>(&must_get_valid_record(prev.clone())?, SETTLEMENT_RUN_ENTRY_INDEX)? {
            Some(p) => p.locked,
            None => return invalid("the run's prev_run is not a settlement run"),
        },
        None => match escrow.terms.initial_lock(market) {
            Ok(lock) => lock,
            Err(e) => return invalid(e.to_string()),
        },
    };
    let sold = match run_sold_lots(&escrow.terms, market, &prev_locked, &run.locked, run.mode) {
        Ok(lots) => lots,
        Err(e) => return invalid(e.to_string()),
    };
    ensure!(sold > 0, "only a run that sold lots is a trade");
    let expected = TradeTag {
        price_per_lot: escrow.terms.price_per_lot,
        lots: sold,
        maker_side: escrow.terms.side,
        run_ts,
        escrow: run.escrow.get_raw_39().to_vec(),
    };
    ensure!(tag == expected, "trade index tag does not match the run");
    valid()
}

/// The entry of `record` if it is a `Create` of `ledger_integrity`'s entry
/// type at `entry_index`.
fn ledger_entry<T>(record: &Record, entry_index: u8) -> ExternResult<Option<T>>
where
    T: TryFrom<SerializedBytes, Error = SerializedBytesError>,
{
    let ActionData::Create(create) = &record.action().data else {
        return Ok(None);
    };
    let EntryType::App(def) = &create.entry_type else {
        return Ok(None);
    };
    // `zome_names` is in zome-index order, integrity zomes first.
    let ledger = ZomeName::from(LEDGER_INTEGRITY_ZOME);
    let Some(ledger_index) = dna_info()?.zome_names.iter().position(|z| z == &ledger) else {
        return Err(wasm_error!(WasmErrorInner::Guest(format!("{LEDGER_INTEGRITY_ZOME} is not in this DNA"))));
    };
    if usize::from(def.zome_index.0) != ledger_index || def.entry_index.0 != entry_index {
        return Ok(None);
    }
    // A valid Create of a public entry always carries its entry.
    let Some(Entry::App(bytes)) = record.entry().as_option() else {
        return Err(wasm_error!(WasmErrorInner::Guest("ledger record has no app entry".into())));
    };
    Ok(Some(T::try_from(bytes.clone().into_sb()).map_err(|e| wasm_error!(e))?))
}
