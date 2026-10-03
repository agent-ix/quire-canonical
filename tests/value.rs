// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Agent-IX
//! `serde_json::Value` through its iterative `Encode` impl (the `serde_json`
//! feature): the same bytes as the serde path and the golden vectors, any
//! depth on a small thread stack, the numbers it refuses, and `drop_value`.
//!
//! `make test` runs this file twice: with serde_json's default
//! `BTreeMap`-backed `Map`, and with `preserve_order`, where a map iterates in
//! insertion order and the encoder alone puts members in canonical order.

#![cfg(feature = "serde_json")]

use quire_canonical::{drop_value, read, sha256, to_vec, Error, FixedShape, Limits};
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

/// Whether serde_json was built with `arbitrary_precision`, the only mode in
/// which a `Number` holds an integer past 64 bits.
fn arbitrary_precision() -> bool {
    Number::from_u128(1 << 64).is_some()
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
        let bytes = to_vec(&value, LIMITS).expect("encodes");
        assert_eq!(String::from_utf8_lossy(&bytes), expected, "{value}");
        match to_vec(&ViaSerde(&value), LIMITS) {
            Ok(serde) => assert_eq!(bytes, serde, "{value}"),
            // With serde_json's `arbitrary_precision` a `Number` serializes as
            // a private token, which the serde path refuses by design.
            Err(Error::SerdeJsonPrivateToken(_)) if arbitrary_precision() => {}
            Err(error) => panic!("{value}: serde path refused: {error}"),
        }
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

/// The canonical text `read` gives for `text`.
fn read_canonical(text: &str) -> String {
    let document = read(text.as_bytes(), u64::MAX).expect("reads");
    String::from_utf8(to_vec(&document, LIMITS).expect("encodes")).expect("UTF-8")
}

/// A float literal gives the same bytes through a `Value` as through `read`:
/// serde_json, built with `float_roundtrip`, parses it to its nearest double,
/// as the reader does. Without that feature serde_json rounds
/// `9007199254740993.0` twice, to `9007199254740994`.
#[test]
fn float_literals_give_the_same_bytes_through_a_value_as_through_read() {
    for (text, expected) in [
        ("9007199254740993.0", "9007199254740992"),
        ("123456789.87654321", "123456789.87654321"),
        ("2.2250738585072011e-308", "2.225073858507201e-308"),
        ("1.7976931348623157e308", "1.7976931348623157e+308"),
        ("0.30000000000000004", "0.30000000000000004"),
    ] {
        let value: Value = serde_json::from_str(text).expect("parses");
        assert_eq!(canonical(&value), expected, "{text}");
        assert_eq!(read_canonical(text), expected, "{text}");
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

/// Build, encode and drop a value on a 64 KiB stack; return its bytes. The
/// value goes through `drop_value`: serde_json's own `Drop` would overflow.
fn encode_on_small_stack(build: fn() -> Value) -> String {
    let bytes = on_small_stack(move || {
        let value = build();
        let bytes = to_vec(&value, Limits::new(u64::MAX)).expect("encodes at any depth");
        drop_value(value);
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

/// `drop_value` drops a 100,000-deep array and a 100,000-deep object on a
/// 64 KiB stack. serde_json's own recursive `Drop` overflows that stack, and
/// a stack overflow aborts the whole test process, so the drops returning is
/// the check.
#[test]
fn drop_value_drops_100000_deep_values_on_a_64_kib_stack() {
    on_small_stack(|| {
        drop_value(deep_array());
        drop_value(deep_object());
    });
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

/// A `Value`'s integers follow the integer rule, not `read`'s rule for JSON
/// text. A literal serde_json holds as an `i64` or `u64` past `2^53` is
/// refused through a `Value`, while `read` encodes its double. A literal past
/// the 64-bit range is a float to serde_json and encodes as its double
/// through both, except with `arbitrary_precision`, where the `Value` path
/// keeps it an integer and refuses it.
#[test]
fn integer_literals_follow_the_integer_rule_only_within_64_bits() {
    for (text, refused, double) in [
        (
            "9007199254740993",
            9_007_199_254_740_993_i128,
            "9007199254740992",
        ),
        (
            "-9007199254740993",
            -9_007_199_254_740_993,
            "-9007199254740992",
        ),
        (
            "18446744073709551615",
            i128::from(u64::MAX),
            "18446744073709552000",
        ),
        (
            "-9223372036854775808",
            i128::from(i64::MIN),
            "-9223372036854776000",
        ),
    ] {
        let value: Value = serde_json::from_str(text).expect("parses");
        match to_vec(&value, LIMITS) {
            Err(Error::IntegerMagnitudeAboveMaximum(found)) => assert_eq!(found, refused),
            other => panic!("{text}: expected refusal, got {other:?}"),
        }
        assert_eq!(read_canonical(text), double, "{text}");
    }
    for (text, double) in [
        ("18446744073709551616", "18446744073709552000"),
        ("-9223372036854775809", "-9223372036854776000"),
        ("100000000000000000001", "100000000000000000000"),
    ] {
        let value: Value = serde_json::from_str(text).expect("parses");
        if arbitrary_precision() {
            match to_vec(&value, LIMITS) {
                Err(Error::WideIntegerMagnitudeAboveMaximum(found)) => assert_eq!(found, text),
                other => panic!("{text}: expected refusal, got {other:?}"),
            }
        } else {
            assert_eq!(canonical(&value), double, "{text}");
        }
        assert_eq!(read_canonical(text), double, "{text}");
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
    if arbitrary_precision() {
        let infinite = infinite.expect("arbitrary_precision keeps the literal");
        assert!(matches!(
            to_vec(&infinite, LIMITS),
            Err(Error::NonFiniteNumber(found)) if found == f64::INFINITY
        ));
        let beyond_u64 = Number::from_u128(1 << 64).expect("arbitrary_precision");
        for (value, text) in [
            (Value::Number(beyond_u64), "18446744073709551616"),
            (serde_json::from_str(&wide).expect("parses"), wide.as_str()),
        ] {
            match to_vec(&value, LIMITS) {
                Err(Error::WideIntegerMagnitudeAboveMaximum(found)) => assert_eq!(found, text),
                other => panic!("{value}: expected refusal, got {other:?}"),
            }
        }
    } else {
        assert!(infinite.is_err(), "1e400 has no finite double");
        assert!(Number::from_f64(f64::INFINITY).is_none());
        assert!(Number::from_f64(f64::NAN).is_none());
    }
}
