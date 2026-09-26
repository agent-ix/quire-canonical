// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Agent-IX
//! The serde data model onto RFC 8785, and the values with no encoding.

use std::collections::BTreeMap;
use std::fmt;
use std::io;

use quire_canonical::{
    encode, sha256, sha256_with_domain, to_vec, Error, Limits, ProtocolViolation, WriteSink,
};
use serde::ser::{SerializeMap, SerializeStruct, Serializer};
use serde::Serialize;
use serde_json::Value;

const LIMITS: Limits = match Limits::new(1 << 16, 32) {
    Ok(limits) => limits,
    Err(_) => panic!("depth within MAX_DEPTH"),
};

fn canonical<T: Serialize + ?Sized>(value: &T) -> String {
    String::from_utf8(to_vec(value, LIMITS).expect("encodes")).expect("UTF-8")
}

#[derive(Serialize)]
enum Shape {
    Unit,
    Newtype(u8),
    Tuple(u8, &'static str),
    Struct { width: u8, height: u8 },
}

#[test]
fn enums_use_the_external_tag_with_sorted_struct_fields() {
    assert_eq!(canonical(&Shape::Unit), "\"Unit\"");
    assert_eq!(canonical(&Shape::Newtype(7)), "{\"Newtype\":7}");
    assert_eq!(canonical(&Shape::Tuple(1, "a")), "{\"Tuple\":[1,\"a\"]}");
    assert_eq!(
        canonical(&Shape::Struct {
            width: 2,
            height: 3
        }),
        "{\"Struct\":{\"height\":3,\"width\":2}}"
    );
}

#[test]
fn scalars_options_and_bytes() {
    assert_eq!(canonical(&Option::<u8>::None), "null");
    assert_eq!(canonical(&Some(1.5_f64)), "1.5");
    assert_eq!(canonical(&()), "null");
    assert_eq!(canonical(&'"'), "\"\\\"\"");
    assert_eq!(canonical(&0.1_f32), "0.10000000149011612");
    assert_eq!(canonical(&serde_bytes_like(&[0, 255])), "[0,255]");
    assert_eq!(canonical(&-0.0_f64), "0");
}

#[test]
fn integer_keys_are_names_sorted_as_text() {
    let map: BTreeMap<u32, bool> = [(2, true), (10, false)].into_iter().collect();
    assert_eq!(canonical(&map), "{\"10\":false,\"2\":true}");
}

#[test]
fn non_string_keys_are_refused() {
    let map: BTreeMap<bool, u8> = [(true, 1)].into_iter().collect();
    assert!(matches!(
        to_vec(&map, LIMITS),
        Err(Error::NonStringMemberName { found: "bool" })
    ));
    let map: BTreeMap<(u8, u8), u8> = [((1, 2), 3)].into_iter().collect();
    assert!(matches!(
        to_vec(&map, LIMITS),
        Err(Error::NonStringMemberName { found: "tuple" })
    ));
}

/// A map that repeats a name.
struct Duplicated;

impl Serialize for Duplicated {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(Some(3))?;
        map.serialize_entry("b", &1)?;
        map.serialize_entry("a", &2)?;
        map.serialize_entry("b", &3)?;
        map.end()
    }
}

#[test]
fn duplicate_member_names_are_refused() {
    match to_vec(&Duplicated, LIMITS) {
        Err(Error::DuplicateMemberName { name }) => assert_eq!(name, "b"),
        other => panic!("expected duplicate refusal, got {other:?}"),
    }
}

/// PLAT-1074: every integer past `2^53` (9007199254740992) in magnitude is
/// refused, exact double or not — there is no mode that accepts a larger
/// integer.
#[test]
fn integers_past_two_pow_53_in_magnitude_are_refused() {
    const MAX_EXACT_MAGNITUDE: u64 = 9_007_199_254_740_992;

    // At the bound, both signs: accepted.
    assert_eq!(canonical(&MAX_EXACT_MAGNITUDE), "9007199254740992");
    assert_eq!(
        canonical(&-(MAX_EXACT_MAGNITUDE as i64)),
        "-9007199254740992"
    );

    // One past the bound, both signs: refused, naming the value.
    assert!(matches!(
        to_vec(&(MAX_EXACT_MAGNITUDE + 1), LIMITS),
        Err(Error::IntegerMagnitudeAboveMaximum(9_007_199_254_740_993))
    ));
    assert!(matches!(
        to_vec(&(-(MAX_EXACT_MAGNITUDE as i64) - 1), LIMITS),
        Err(Error::IntegerMagnitudeAboveMaximum(-9_007_199_254_740_993))
    ));

    // Exact as a double (a single set bit, far past the bound) but still
    // refused: exactness alone is no longer enough.
    assert!(matches!(
        to_vec(&(1_u128 << 100), LIMITS),
        Err(Error::UnsignedIntegerMagnitudeAboveMaximum(_))
    ));

    // i64::MAX and u64::MAX: both far past the bound, both refused.
    assert!(matches!(
        to_vec(&i64::MAX, LIMITS),
        Err(Error::IntegerMagnitudeAboveMaximum(_))
    ));
    assert!(matches!(
        to_vec(&u64::MAX, LIMITS),
        Err(Error::IntegerMagnitudeAboveMaximum(_))
    ));
    assert!(matches!(
        to_vec(&u128::MAX, LIMITS),
        Err(Error::UnsignedIntegerMagnitudeAboveMaximum(u128::MAX))
    ));

    // PLAT-1074 SR-002 FND-001: serialize_i128 specifically, at the bound
    // (both signs, accepted) and past it (both signs, refused). If
    // serialize_i128 ever bypassed the check (e.g. `self.double(value as
    // f64)`), `-(1_i128 << 60)` would silently encode instead of refusing.
    let at_bound_positive: i128 = 1_i128 << 53;
    let at_bound_negative: i128 = -(1_i128 << 53);
    assert_eq!(canonical(&at_bound_positive), "9007199254740992");
    assert_eq!(canonical(&at_bound_negative), "-9007199254740992");
    assert!(matches!(
        to_vec(&(-(1_i128 << 60)), LIMITS),
        Err(Error::IntegerMagnitudeAboveMaximum(value)) if value == -(1_i128 << 60)
    ));
    assert!(matches!(
        to_vec(&i128::MIN, LIMITS),
        Err(Error::IntegerMagnitudeAboveMaximum(i128::MIN))
    ));
}

/// PLAT-1074: `serde_json::Number` reaches the encoder through whichever of
/// `serialize_i64`, `serialize_u64` or `serialize_f64` its internal
/// representation picks (`N::NegInt`, `N::PosInt`, `N::Float`); every path
/// enforces the same `2^53` magnitude bound.
#[test]
fn serde_json_number_paths_all_enforce_the_magnitude_bound() {
    use serde_json::json;

    const MAX_EXACT_MAGNITUDE: u64 = 9_007_199_254_740_992;

    // serialize_u64 path (serde_json::Number::PosInt), at and past the bound.
    assert_eq!(canonical(&json!(MAX_EXACT_MAGNITUDE)), "9007199254740992");
    assert!(matches!(
        to_vec(&json!(MAX_EXACT_MAGNITUDE + 1), LIMITS),
        Err(Error::IntegerMagnitudeAboveMaximum(9_007_199_254_740_993))
    ));
    assert!(matches!(
        to_vec(&json!(u64::MAX), LIMITS),
        Err(Error::IntegerMagnitudeAboveMaximum(_))
    ));

    // serialize_i64 path (serde_json::Number::NegInt), at and past the bound.
    assert_eq!(
        canonical(&json!(-(MAX_EXACT_MAGNITUDE as i64))),
        "-9007199254740992"
    );
    assert!(matches!(
        to_vec(&json!(-(MAX_EXACT_MAGNITUDE as i64) - 1), LIMITS),
        Err(Error::IntegerMagnitudeAboveMaximum(-9_007_199_254_740_993))
    ));
    assert!(matches!(
        to_vec(&json!(i64::MIN), LIMITS),
        Err(Error::IntegerMagnitudeAboveMaximum(_))
    ));

    // serialize_f64 path (serde_json::Number::Float): unaffected, still a
    // plain double, whatever its magnitude.
    assert_eq!(canonical(&json!(1.5)), "1.5");
    assert_eq!(canonical(&json!(1e300)), "1e+300");

    // The same values parsed from JSON text, still within u64/i64 range, take
    // the identical serialize_u64/serialize_i64 path as the `json!` macro
    // above, so the same bound applies.
    let parsed: Value = serde_json::from_str("9007199254740992").expect("valid JSON");
    assert_eq!(canonical(&parsed), "9007199254740992");
    let parsed: Value = serde_json::from_str("9007199254740993").expect("valid JSON");
    assert!(matches!(
        to_vec(&parsed, LIMITS),
        Err(Error::IntegerMagnitudeAboveMaximum(9_007_199_254_740_993))
    ));
}

/// PLAT-1074 SR-001 FND-001: a JSON-text integer literal already too large
/// for `u64`/`i64` is not one of the Rust integer types the magnitude bound
/// applies to. `serde_json` itself parses it straight to an `f64`
/// (`serde_json::Number::Float`) before this crate ever sees it, so it
/// reaches the encoder as a plain double and is encoded as one — silently
/// rounded to the nearest double, exactly like any other out-of-range float,
/// and never refused. This is deliberate, not a gap: RFC 8785 has no
/// exact-integer mode, and refusing every integral double past `2^53` would
/// also refuse `1e+21`, which RFC 8785 explicitly allows.
#[test]
fn json_text_integers_beyond_u64_take_the_float_path_and_are_not_refused() {
    // One past u64::MAX (18446744073709551615): still parses as a float, not
    // a refusal, and rounds to the nearest double.
    let parsed: Value =
        serde_json::from_str("18446744073709551617").expect("valid JSON, parses as f64");
    assert_eq!(canonical(&parsed), "18446744073709552000");

    // One past i64::MIN's magnitude on the negative side: the same.
    let parsed: Value =
        serde_json::from_str("-9223372036854775809").expect("valid JSON, parses as f64");
    assert_eq!(canonical(&parsed), "-9223372036854776000");

    // A round number whose nearest double happens to print without rounding
    // noise: still accepted, still a float.
    let parsed: Value =
        serde_json::from_str("100000000000000000001").expect("valid JSON, parses as f64");
    assert_eq!(canonical(&parsed), "100000000000000000000");
}

/// What `serde_json::Number` looks like on the wire under
/// `arbitrary_precision`.
struct ArbitraryPrecisionNumber;

impl Serialize for ArbitraryPrecisionNumber {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut token = serializer.serialize_struct("$serde_json::private::Number", 1)?;
        token.serialize_field("$serde_json::private::Number", "1.0")?;
        token.end()
    }
}

#[test]
fn serde_json_private_tokens_are_refused_not_encoded_as_objects() {
    assert!(matches!(
        to_vec(&ArbitraryPrecisionNumber, LIMITS),
        Err(Error::SerdeJsonPrivateToken("$serde_json::private::Number"))
    ));
}

/// A `Display` value serialized through `collect_str`.
struct Shown;

impl fmt::Display for Shown {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Two writes, so the escaper sees more than one fragment.
        formatter.write_str("say \"hi\"\n")?;
        formatter.write_str("\u{1}")
    }
}

impl Serialize for Shown {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

#[test]
fn collect_str_is_escaped_and_metered() {
    assert_eq!(canonical(&Shown), "\"say \\\"hi\\\"\\n\\u0001\"");
    assert!(matches!(
        to_vec(&Shown, Limits::new(5, 1).expect("valid")),
        Err(Error::Limit(_))
    ));
}

#[test]
fn domain_digest_is_length_prefixed_label_then_canonical_bytes() {
    use sha2::Digest as _;
    let value = BTreeMap::from([("k", 1)]);
    let digest = sha256_with_domain(b"domain", &value, LIMITS).expect("hashes");
    let expected: [u8; 32] = sha2::Sha256::new()
        .chain_update(6_u64.to_be_bytes())
        .chain_update(b"domain{\"k\":1}")
        .finalize()
        .into();
    assert_eq!(digest.as_bytes(), &expected);
    assert_ne!(sha256(&value, LIMITS).expect("hashes"), digest);
}

/// FND-004: without the length prefix, `"tag"` + `12` and `"tag1"` + `2` both
/// hashed `tag12`.
#[test]
fn domain_label_and_text_cannot_be_reassociated() {
    let first = sha256_with_domain(b"tag", &12_u8, LIMITS).expect("hashes");
    let second = sha256_with_domain(b"tag1", &2_u8, LIMITS).expect("hashes");
    assert_ne!(first, second);
    let empty = sha256_with_domain(b"", &1_u8, LIMITS).expect("hashes");
    assert_ne!(empty, sha256(&1_u8, LIMITS).expect("hashes"));
}

/// A map driven through the `SerializeMap` calls in `steps`.
struct Driven(&'static [Step]);

#[derive(Clone, Copy)]
enum Step {
    Key(&'static str),
    Value(u8),
    Entry(&'static str, u8),
}

impl Serialize for Driven {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(None)?;
        for step in self.0 {
            match *step {
                Step::Key(key) => map.serialize_key(key)?,
                Step::Value(value) => map.serialize_value(&value)?,
                Step::Entry(key, value) => map.serialize_entry(key, &value)?,
            }
        }
        map.end()
    }
}

fn violation(steps: &'static [Step]) -> Option<ProtocolViolation> {
    let mut sink = Vec::new();
    match encode(&mut sink, &Driven(steps), LIMITS) {
        Err(Error::Protocol(violation)) => Some(violation),
        Ok(written) => panic!(
            "accepted {:?} ({written} bytes)",
            String::from_utf8_lossy(&sink)
        ),
        Err(other) => panic!("unexpected error {other:?}"),
    }
}

/// FND-001: two keys in a row used to produce `{"a":"b":1}` as success.
#[test]
fn a_second_key_before_a_value_is_refused() {
    assert_eq!(
        violation(&[Step::Key("a"), Step::Key("b"), Step::Value(1)]),
        Some(ProtocolViolation::NameWithoutValue)
    );
}

/// FND-001: a trailing key used to be dropped silently, with `encode`
/// reporting more bytes than it wrote.
#[test]
fn ending_a_map_after_a_key_is_refused() {
    assert_eq!(
        violation(&[Step::Entry("z", 1), Step::Key("a")]),
        Some(ProtocolViolation::ObjectEndedAfterName)
    );
}

/// FND-007: a value with no key is refused before it is written.
#[test]
fn a_value_without_a_key_is_refused() {
    assert_eq!(
        violation(&[Step::Value(1)]),
        Some(ProtocolViolation::ValueWithoutName)
    );
    assert_eq!(
        violation(&[Step::Entry("a", 1), Step::Value(2)]),
        Some(ProtocolViolation::ValueWithoutName)
    );
}

#[test]
fn well_ordered_key_value_calls_still_encode() {
    let mut sink = Vec::new();
    let written = encode(
        &mut sink,
        &Driven(&[Step::Key("b"), Step::Value(1), Step::Entry("a", 2)]),
        LIMITS,
    )
    .expect("encodes");
    assert_eq!(sink, b"{\"a\":2,\"b\":1}");
    assert_eq!(written, u64::try_from(sink.len()).expect("fits"));
}

/// A writer that refuses everything.
struct Broken;

impl io::Write for Broken {
    fn write(&mut self, _bytes: &[u8]) -> io::Result<usize> {
        Err(io::Error::other("disk full"))
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[test]
fn sink_failure_is_a_refusal() {
    assert!(matches!(
        encode(&mut WriteSink(Broken), &1_u8, LIMITS),
        Err(Error::Sink(_))
    ));
}

/// Bytes serialized through `serialize_bytes`.
struct Bytes<'a>(&'a [u8]);

impl Serialize for Bytes<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_bytes(self.0)
    }
}

fn serde_bytes_like(bytes: &[u8]) -> Bytes<'_> {
    Bytes(bytes)
}
