// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Agent-IX
//! `serde_json::Value` through its iterative `Encode` impl (the `serde_json`
//! feature): the same bytes as the serde path and the golden vectors, any
//! depth on a small thread stack, and the numbers it refuses.
//!
//! `make test` runs this file in its `test-preserve-order` lane, which turns
//! the feature on. There `serde_json::Map` iterates in insertion order, so the
//! encoder alone puts members in canonical order.

#![cfg(feature = "serde_json")]

use quire_canonical::{sha256, to_vec, Error, FixedShape, Limits};
use serde::Serialize;
use serde_json::{json, Map, Number, Value};

const LIMITS: Limits = Limits::new(1 << 20);
const DEEP: usize = 100_000;
const SMALL_STACK: usize = 64 * 1024;
const VECTORS: &str = include_str!("vectors/jcs-vectors.json");

/// A `Value` through the crate's serde path: serde_json's own recursive
/// `Serialize` driving the serde encoder. The literal `DEPTH` defeats the
/// recursion check (see `FixedShape`); that is safe here only because every
/// value this test wraps is shallow.
struct ViaSerde<'a>(&'a Value);

impl FixedShape for ViaSerde<'_> {
    const DEPTH: usize = 0;
}

impl Serialize for ViaSerde<'_> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.0.serialize(serializer)
    }
}

fn canonical(value: &Value) -> String {
    String::from_utf8(to_vec(value, LIMITS).expect("encodes")).expect("UTF-8")
}

/// Ordinary values encode to their RFC 8785 text, byte for byte the same as
/// the serde path gives for the same `Value`: member names in UTF-16 order at
/// every level (supplementary-plane names before U+E000..U+FFFF ones),
/// integers at plus and minus `2^53`, ECMAScript number text, escapes, and
/// empty containers.
#[test]
fn ordinary_values_match_the_serde_path_and_their_canonical_text() {
    let cases = [
        (
            json!({
                "\u{FFFD}": {"\u{10000}": 1, "\u{E000}": 2, "\u{E9}": 3, "z": 4},
                "\u{1F600}": "grinning",
                "a": {"\u{65E5}\u{672C}": [], "~": {}},
            }),
            "{\"a\":{\"~\":{},\"\u{65E5}\u{672C}\":[]},\"\u{1F600}\":\"grinning\",\
             \"\u{FFFD}\":{\"z\":4,\"\u{E9}\":3,\"\u{10000}\":1,\"\u{E000}\":2}}",
        ),
        (
            json!([
                9_007_199_254_740_992_u64,
                -9_007_199_254_740_992_i64,
                9_007_199_254_740_991_u64,
                0,
                -1
            ]),
            "[9007199254740992,-9007199254740992,9007199254740991,0,-1]",
        ),
        (
            json!([1e21, 1e-7, -0.0, 5e-324, 1e20, 0.1, 2.0, -1.5e-10]),
            "[1e+21,1e-7,0,5e-324,100000000000000000000,0.1,2,-1.5e-10]",
        ),
        (
            json!({"esc": "\u{0}\u{8}\u{1F}\"\\/\u{7F}\n\t\u{2028}", "\n": "line feed"}),
            concat!(
                r#"{"\n":"line feed","esc":"\u0000\b\u001f\"\\/"#,
                "\u{7F}",
                r#"\n\t"#,
                "\u{2028}",
                r#""}"#
            ),
        ),
        (
            json!({"o": {}, "a": [], "n": [[], {}, [{}]]}),
            r#"{"a":[],"n":[[],{},[{}]],"o":{}}"#,
        ),
        (json!({}), "{}"),
        (json!([]), "[]"),
        (json!(null), "null"),
        (json!(true), "true"),
        (json!("caf\u{E9}"), "\"caf\u{E9}\""),
    ];
    for (value, expected) in cases {
        assert_eq!(canonical(&value), expected, "{value}");
        assert_eq!(
            to_vec(&value, LIMITS).expect("iterative"),
            to_vec(&ViaSerde(&value), LIMITS).expect("serde path"),
            "{value}"
        );
    }
}

/// Every golden vector's preimage, as the `Value` serde_json parses it to,
/// encodes to the vector's canonical bytes and digest.
#[test]
fn every_golden_vector_preimage_encodes_as_a_value() {
    let file: Value = serde_json::from_str(VECTORS).expect("vector file is JSON");
    let vectors = file["vectors"].as_array().expect("vectors array");
    assert!(!vectors.is_empty());
    for vector in vectors {
        let name = vector["name"].as_str().expect("name");
        let preimage = &vector["preimage"];
        let expected = vector["canonical"].as_str().expect("canonical");
        assert_eq!(canonical(preimage), expected, "{name}");
        let digest = sha256(preimage, LIMITS).expect("digests");
        assert_eq!(
            digest.to_string(),
            vector["sha256"].as_str().expect("sha256"),
            "{name}"
        );
    }
}

/// Run `work` on a thread with a 64 KiB stack.
fn on_small_stack<T: Send + 'static>(work: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::Builder::new()
        .stack_size(SMALL_STACK)
        .spawn(work)
        .expect("spawn")
        .join()
        .expect("no stack overflow")
}

/// Drop `value` from a heap stack. serde_json's own `Drop` recurses once per
/// level, so a deep value dropped natively would overflow the small stack:
/// each node's children move onto the heap stack before the node is dropped.
fn dismantle(value: Value) {
    let mut stack = vec![value];
    while let Some(value) = stack.pop() {
        match value {
            Value::Array(items) => stack.extend(items),
            Value::Object(members) => stack.extend(members.into_iter().map(|(_, member)| member)),
            Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
        }
    }
}

/// `DEEP` nested arrays, built from the inside out without recursion.
fn deep_array() -> Value {
    let mut value = Value::Array(Vec::new());
    for _ in 1..DEEP {
        value = Value::Array(vec![value]);
    }
    value
}

/// `DEEP` nested objects, `{"k":{"k":...{"k":null}}}`, built from the inside
/// out without recursion.
fn deep_object() -> Value {
    let mut value = Value::Null;
    for _ in 0..DEEP {
        let mut members = Map::new();
        members.insert("k".to_owned(), value);
        value = Value::Object(members);
    }
    value
}

/// Build, encode and drop a value on a 64 KiB stack; return its bytes.
fn encode_on_small_stack(build: fn() -> Value) -> String {
    let bytes = on_small_stack(move || {
        let value = build();
        let bytes = to_vec(&value, Limits::new(u64::MAX)).expect("encodes at any depth");
        dismantle(value);
        bytes
    });
    String::from_utf8(bytes).expect("UTF-8")
}

/// A 100,000-deep array encodes on a 64 KiB stack, where a walk that
/// recursed once per level would overflow.
#[test]
fn a_100000_deep_array_encodes_on_a_64_kib_stack() {
    let expected = format!("{}{}", "[".repeat(DEEP), "]".repeat(DEEP));
    assert_eq!(encode_on_small_stack(deep_array), expected);
}

/// A 100,000-deep object encodes on a 64 KiB stack, where a walk that
/// recursed once per level would overflow.
#[test]
fn a_100000_deep_object_encodes_on_a_64_kib_stack() {
    let expected = format!("{}null{}", r#"{"k":"#.repeat(DEEP), "}".repeat(DEEP));
    assert_eq!(encode_on_small_stack(deep_object), expected);
}

/// An integer past `2^53`, from an `i64` or a `u64`, at the top level or
/// nested, is refused naming its value, like any Rust integer.
#[test]
fn integers_past_two_pow_53_are_refused() {
    let cases = [
        (json!(9_007_199_254_740_993_u64), 9_007_199_254_740_993_i128),
        (json!(-9_007_199_254_740_993_i64), -9_007_199_254_740_993),
        (json!(u64::MAX), i128::from(u64::MAX)),
        (json!(i64::MIN), i128::from(i64::MIN)),
    ];
    for (value, refused) in cases {
        let nested = json!({"b": true, "a": [1, value.clone()]});
        for value in [value, nested] {
            match to_vec(&value, LIMITS) {
                Err(Error::IntegerMagnitudeAboveMaximum(found)) => assert_eq!(found, refused),
                other => panic!("{value}: expected refusal, got {other:?}"),
            }
        }
    }
}

/// Without serde_json's `arbitrary_precision` feature a `Value` cannot hold a
/// non-finite number or an integer too wide for 64 bits. With it, both can be
/// built, and both are refused: the infinite literal as a non-finite number,
/// the wide integer naming its decimal text.
#[test]
fn numbers_only_arbitrary_precision_holds_are_refused() {
    let infinite = serde_json::from_str::<Value>("[1e400]");
    let wide = "1".to_owned() + &"0".repeat(40);
    match Number::from_u128(1 << 64) {
        Some(beyond_u64) => {
            let infinite = infinite.expect("arbitrary_precision keeps the literal");
            assert!(matches!(
                to_vec(&infinite, LIMITS),
                Err(Error::NonFiniteNumber(found)) if found == f64::INFINITY
            ));
            for (value, text) in [
                (Value::Number(beyond_u64), "18446744073709551616"),
                (serde_json::from_str(&wide).expect("parses"), wide.as_str()),
            ] {
                match to_vec(&value, LIMITS) {
                    Err(Error::WideIntegerMagnitudeAboveMaximum(found)) => assert_eq!(found, text),
                    other => panic!("{value}: expected refusal, got {other:?}"),
                }
            }
        }
        None => {
            assert!(infinite.is_err(), "1e400 has no finite double");
            assert!(Number::from_f64(f64::INFINITY).is_none());
            assert!(Number::from_f64(f64::NAN).is_none());
        }
    }
}
