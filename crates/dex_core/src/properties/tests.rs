use super::*;
use crate::markets::MarketDef;

fn market(base: &str, quote: &str) -> MarketDef {
    MarketDef { base: base.into(), quote: quote.into(), lot_size: 100, tick_size: 1 }
}

fn with(f: impl FnOnce(&mut DexProperties)) -> Result<(), PropertiesError> {
    let mut p = DexProperties::demo();
    f(&mut p);
    p.check()
}

#[test]
fn the_demo_properties_are_valid() {
    let p = DexProperties::demo();
    assert_eq!(p.check(), Ok(()));
    assert_eq!(p.timing().unwrap(), Timing { park_timeout_us: 1_800_000_000, settle_grace_us: 300_000_000 });
    assert_eq!(p.market(&MarketDef::default_pair().id()).unwrap(), &MarketDef::default_pair());
}

#[test]
fn genesis_rules_refuse_every_listed_mistake() {
    let b = || UnitDef { id: "B".into(), decimals: 2 };
    assert_eq!(with(|p| p.park_timeout_secs = 0), Err(PropertiesError::ZeroParkTimeout));
    assert_eq!(with(|p| p.max_order_lifetime_secs = 0), Err(PropertiesError::ZeroOrderLifetime));
    assert_eq!(with(|p| p.max_order_lifetime_secs = u64::MAX), Err(PropertiesError::DurationOverflow));
    assert_eq!(with(|p| p.park_timeout_secs = u64::MAX), Err(PropertiesError::DurationOverflow));
    assert_eq!(with(|p| p.units.clear()), Err(PropertiesError::NoUnits));
    assert_eq!(with(|p| p.units.push(UnitDef { id: "A".into(), decimals: 2 })), Err(PropertiesError::DuplicateUnit("A".into())));
    assert_eq!(with(|p| p.units.push(UnitDef { id: "a b".into(), decimals: 2 })), Err(PropertiesError::BadUnitId("a b".into())));
    assert_eq!(with(|p| p.units.push(UnitDef { id: "A\0".into(), decimals: 2 })), Err(PropertiesError::BadUnitId("A\0".into())));
    assert_eq!(with(|p| p.units.push(UnitDef { id: "Z".into(), decimals: 19 })), Err(PropertiesError::TooManyDecimals("Z".into())));
    assert_eq!(with(|p| p.units.retain(|u| u.id != HUB_UNIT)), Err(PropertiesError::NoHubUnit));
    assert_eq!(with(|p| p.markets.clear()), Err(PropertiesError::NoMarkets));
    assert_eq!(with(|p| p.markets.push(market("HF", "HF"))), Err(PropertiesError::BaseIsQuote("HF".into())));
    assert_eq!(with(|p| p.markets.push(market("Q", "HF"))), Err(PropertiesError::UndefinedUnit("Q".into())));
    // Every market is X/HF.
    assert_eq!(
        with(|p| { p.units.push(b()); p.markets.push(market("A", "B")) }),
        Err(PropertiesError::NotQuotedInHub { base: "A".into(), quote: "B".into() })
    );
    assert_eq!(with(|p| p.markets[0].lot_size = 0), Err(PropertiesError::ZeroLotSize(MarketDef::default_pair().id())));
    assert_eq!(with(|p| p.markets[0].tick_size = 0), Err(PropertiesError::ZeroTickSize(MarketDef::default_pair().id())));
    // The same pair with other parameters is the same id.
    assert_eq!(
        with(|p| p.markets.push(MarketDef { lot_size: 10, ..MarketDef::default_pair() })),
        Err(PropertiesError::DuplicateMarket(MarketDef::default_pair().id()))
    );
    // A pair and its inverse: refused (by the hub rule first, as HF/A is not quoted in HF).
    assert!(with(|p| p.markets.push(market("HF", "A"))).is_err());
}

#[test]
fn a_second_market_quoted_in_hf_is_fine() {
    assert_eq!(
        with(|p| {
            p.units.push(UnitDef { id: "C".into(), decimals: 0 });
            p.markets.push(MarketDef { base: "C".into(), quote: HUB_UNIT.into(), lot_size: 1, tick_size: 5 });
        }),
        Ok(())
    );
}

#[test]
fn amounts_in_undeclared_units_are_refused() {
    let p = DexProperties::demo();
    assert_eq!(p.check_amounts(&Amounts::new(5, 7)), Ok(()));
    assert_eq!(p.check_amounts(&Amounts::of("Z", 1)), Err(PropertiesError::UndefinedUnit("Z".into())));
}

#[test]
fn market_ids_depend_on_the_pair_only_and_round_trip_as_hex() {
    let id = MarketId::of("A", "HF");
    assert_eq!(id, MarketDef::default_pair().id());
    assert_ne!(id, MarketId::of("HF", "A"), "order matters");
    assert_ne!(MarketId::of("AB", "C"), MarketId::of("A", "BC"), "the separator makes the encoding unambiguous");
    assert_eq!(MarketId::from_hex(&id.to_hex()), Some(id));
    assert_eq!(MarketId::from_hex(&id.to_hex().to_uppercase()), None, "one spelling");
    let json = serde_json::to_string(&id).unwrap();
    assert_eq!(serde_json::from_str::<MarketId>(&json).unwrap(), id);
}

#[test]
fn unknown_fields_in_the_properties_are_refused() {
    let json = r#"{"park_timeout_secs":1800,"settle_grace_secs":300,"units":[],"markets":[],"park_timout":1}"#;
    assert!(serde_json::from_str::<DexProperties>(json).is_err());
}
