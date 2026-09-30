//! The template's helpers on their own: the formatter, the amount parser and
//! the timestamp parser, run through the real engine. The harness takes the
//! template up to its `// ---- end of helpers ----` marker, so these test the
//! shipped code, not a copy.

use crate::engine::{fmt, TEMPLATE};
use hdi::prelude::Timestamp;
use rave_engine::prelude::RhaiEngine;
use serde_json::{json, Value};

const MARKER: &str = "// ---- end of helpers ----";

/// Run `body` after the template's helpers; `body` must evaluate to a value
/// that becomes `computed_values`.
fn helpers(body: &str) -> Value {
    let head = &TEMPLATE[..TEMPLATE.find(MARKER).expect("helper marker")];
    let script = format!("{head}\nlet result = {{ {body} }};\n#{{ \"output\": #{{ \"computed_values\": result }} }}");
    let out = RhaiEngine::new()
        .execute(&json!({}), rmp_serde::to_vec(&script).unwrap(), None)
        .unwrap_or_else(|e| panic!("helper script failed: {e:?}"));
    out.output.computed_values.expect("computed_values")
}

fn strings(v: &Value) -> Vec<String> {
    v.as_array().unwrap().iter().map(|s| s.as_str().unwrap().to_string()).collect()
}

const VALUES: [u64; 16] = [
    0, 1, 5, 9, 10, 99, 100, 101, 120, 4_800, 12_345, 100_000_000, 999_999_999,
    1_234_567_890_123, 92_233_720_368_547_758, 9_223_372_036_854_775_807,
];

#[test]
fn formats_exactly_two_decimals_and_never_minus_zero() {
    let list = VALUES.map(|v| v.to_string()).join(", ");
    let out = helpers(&format!("let o = []; for n in [{list}] {{ o.push(fmt_minor(n)); }} o"));
    let expected: Vec<String> = VALUES.iter().map(|&v| fmt(v)).collect();
    assert_eq!(strings(&out), expected);
    for s in strings(&out) {
        assert!(!s.starts_with('-'), "{s}");
        assert_eq!(s.split('.').nth(1).unwrap().len(), 2, "{s}");
    }
    assert_eq!(strings(&out)[0], "0.00");
    assert_eq!(strings(&out)[15], "92233720368547758.07", "i64::MAX");
}

#[test]
fn refuses_to_format_a_negative_or_non_integer() {
    let out = helpers(
        r#"let o = [];
        for v in [-1, -100] { try { fmt_minor(v); o.push("ok"); } catch { o.push("refused"); } }
        try { fmt_minor(1.5); o.push("ok"); } catch { o.push("refused"); }
        o"#,
    );
    assert_eq!(strings(&out), ["refused"; 3]);
}

#[test]
fn round_trips_through_the_parser() {
    let list = VALUES.map(|v| v.to_string()).join(", ");
    let out = helpers(&format!(
        "let o = []; for n in [{list}] {{ o.push(to_minor(fmt_minor(n)) == n); }} o"
    ));
    assert!(out.as_array().unwrap().iter().all(|b| b == &Value::Bool(true)), "{out}");
}

#[test]
fn parses_amounts_strictly() {
    let out = helpers(
        r#"let o = [];
        for s in ["1", "1.2", "1.20", "1.200000", "0.07", "048.5", "30000"] { o.push(to_minor(s)); }
        for s in ["1.234", "-1", "1.2.3", "", ".5", "5.", "abc", "1e3", " 1"] {
            try { to_minor(s); o.push("accepted " + s); } catch { o.push("refused"); }
        }
        o"#,
    );
    let got: Vec<Value> = out.as_array().unwrap().clone();
    assert_eq!(&got[..7], &[json!(100), json!(120), json!(120), json!(120), json!(7), json!(4_850), json!(3_000_000)]);
    // "5." has an empty fraction and is accepted as 5.00 by the grammar? No:
    // all_digits("") is false, so it is refused like the rest.
    assert!(got[7..].iter().all(|v| v == "refused"), "{got:?}");
}

#[test]
fn parses_holochain_timestamps_to_micros() {
    // Timestamp's Display is RFC3339 with 0, 3 or 6 fractional digits.
    let micros: [i64; 9] = [
        0,
        1_000_000,
        1_000_001,
        1_790_812_800_000_000,
        1_790_812_800_123_000,
        1_790_812_800_123_456,
        951_782_400_000_000, // 2000-02-29 (leap day)
        4_102_444_800_000_001,   // 2100-01-01T00:00:00.000001Z
        253_370_764_800_999_999, // 9999-01-01T00:00:00.999999Z
    ];
    let texts: Vec<String> = micros.iter().map(|&m| Timestamp::from_micros(m).to_string()).collect();
    assert!(texts.iter().any(|t| !t.contains('.')), "a whole-second form is covered: {texts:?}");
    assert!(texts.iter().any(|t| t.len() == "2026-10-01T00:00:00.123Z".len()), "a millisecond form is covered");
    let list = texts.iter().map(|t| format!("\"{t}\"")).collect::<Vec<_>>().join(", ");
    let out = helpers(&format!("let o = []; for t in [{list}] {{ o.push(timestamp_micros(t)); }} o"));
    let got: Vec<i64> = out.as_array().unwrap().iter().map(|v| v.as_i64().unwrap()).collect();
    assert_eq!(got, micros, "{texts:?}");
}

#[test]
fn refuses_timestamps_that_are_not_rfc3339() {
    // Holochain prints a timestamp chrono cannot convert as its raw value.
    let raw = Timestamp::from_micros(-1).to_string();
    assert_eq!(raw, "(-1μs)");
    let out = helpers(&format!(
        r#"let o = [];
        for t in ["{raw}", "2026-10-01 00:00:00Z", "2026-10-01T00:00:00", "2026-10-01T00:00:00.Z", "2026-10-01T00:00:00+01:00"] {{
            try {{ timestamp_micros(t); o.push("accepted " + t); }} catch {{ o.push("refused"); }}
        }}
        o"#
    ));
    assert_eq!(strings(&out), ["refused"; 5]);
}
