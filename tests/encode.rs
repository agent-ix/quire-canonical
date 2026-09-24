// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Agent-IX
//! The serde data model onto RFC 8785, and the values with no encoding.

use std::collections::BTreeMap;
use std::fmt;
use std::io;

use quire_canonical::{encode, sha256, sha256_with_prefix, to_vec, Error, Limits, WriteSink};
use serde::ser::{SerializeMap, SerializeStruct, Serializer};
use serde::Serialize;

const LIMITS: Limits = Limits::new(1 << 16, 32);

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

#[test]
fn integers_without_an_exact_double_are_refused() {
    assert_eq!(canonical(&9_007_199_254_740_992_u64), "9007199254740992");
    // Exact as a double, and printed as ECMAScript prints that double.
    assert_eq!(canonical(&(1_u64 << 63)), "9223372036854776000");
    assert_eq!(canonical(&(1_u128 << 100)), "1.2676506002282294e+30");
    assert!(matches!(
        to_vec(&9_007_199_254_740_993_u64, LIMITS),
        Err(Error::InexactInteger(9_007_199_254_740_993))
    ));
    assert!(matches!(
        to_vec(&i64::MAX, LIMITS),
        Err(Error::InexactInteger(_))
    ));
    assert!(matches!(
        to_vec(&u128::MAX, LIMITS),
        Err(Error::InexactUnsignedInteger(u128::MAX))
    ));
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
        to_vec(&Shown, Limits::new(5, 1)),
        Err(Error::Limit(_))
    ));
}

#[test]
fn prefixed_digest_is_the_digest_of_prefix_then_canonical_bytes() {
    use sha2::Digest as _;
    let value = BTreeMap::from([("k", 1)]);
    let prefixed = sha256_with_prefix(b"domain\0", &value, LIMITS).expect("hashes");
    let expected: [u8; 32] = sha2::Sha256::new()
        .chain_update(b"domain\0{\"k\":1}")
        .finalize()
        .into();
    assert_eq!(prefixed.as_bytes(), &expected);
    assert_ne!(sha256(&value, LIMITS).expect("hashes"), prefixed);
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
