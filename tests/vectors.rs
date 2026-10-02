// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Agent-IX
//! The golden vectors in `tests/vectors/jcs-vectors.json`, in the QSpec
//! vector shape: each vector names a preimage, its expected canonical text,
//! and the SHA-256 of that text. Each preimage is read by the crate's own
//! reader and encoded from its tree.
//!
//! Every input is original to this crate; the `sec-*` vectors exercise the
//! properties RFC 8785 sections 3.2.2 and 3.2.3 specify, with their own names
//! and values. The expectations were not produced by this crate: the canonical
//! text comes from an ECMAScript `JSON.stringify` reference run, the digests
//! from an independent SHA-256.
//!
//! `make test` runs this file twice: once with `serde_json`'s default
//! `BTreeMap`-backed `Map` (scalar-value key order) and once with
//! `preserve_order` (file order), so the reader sees each preimage's members
//! in two different orders. The expected bytes are the same both times.

use std::collections::BTreeMap;

#[cfg(feature = "std")]
use quire_canonical::WriteSink;
use quire_canonical::{encode, nest, read, sha256, to_vec, Document, Error, FixedShape, Limits};
use serde::Serialize;
use serde_json::Value;

const VECTORS: &str = include_str!("vectors/jcs-vectors.json");
const LIMITS: Limits = Limits::new(1 << 20);

fn vectors_file() -> Value {
    serde_json::from_str(VECTORS).expect("vector file is JSON")
}

fn field<'a>(entry: &'a Value, name: &str) -> &'a str {
    entry
        .get(name)
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("vector entry lacks string field {name}: {entry}"))
}

fn section<'a>(file: &'a Value, name: &str) -> &'a [Value] {
    file.get(name)
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("vector file lacks array {name}"))
}

/// `preimage` as JSON text, read back by the crate's reader.
fn document(preimage: &Value) -> Document {
    let text = serde_json::to_string(preimage).expect("serializes");
    read(text.as_bytes(), u64::MAX).expect("reads")
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

/// PLAT-987 AC-1, AC-2, AC-3: every vector's preimage encodes to its expected
/// bytes and digest, through every sink, in whichever `Map` order this lane
/// compiles `serde_json` with.
#[test]
fn every_vector_encodes_to_its_canonical_bytes_and_digest() {
    let file = vectors_file();
    let vectors = section(&file, "vectors");
    assert!(!vectors.is_empty());
    for vector in vectors {
        let name = field(vector, "name");
        let preimage = &document(
            vector
                .get("preimage")
                .unwrap_or_else(|| panic!("{name}: no preimage")),
        );
        let canonical = field(vector, "canonical").as_bytes();
        let digest = field(vector, "sha256");

        let bytes = to_vec(preimage, LIMITS).unwrap_or_else(|error| panic!("{name}: {error}"));
        assert_eq!(text(&bytes), text(canonical), "{name}: canonical bytes");

        let hashed = sha256(preimage, LIMITS).unwrap_or_else(|error| panic!("{name}: {error}"));
        assert_eq!(hashed.to_string(), digest, "{name}: sha256");

        // One pass into both sinks agrees with the separate passes.
        let mut both = (Vec::new(), sha2_hasher());
        let length = encode(&mut both, preimage, LIMITS).expect("encodes");
        assert_eq!(both.0, canonical, "{name}: tee bytes");
        assert_eq!(hex(&finalize(both.1)), digest, "{name}: tee digest");
        assert_eq!(length, u64::try_from(canonical.len()).expect("fits"));

        #[cfg(feature = "std")]
        {
            let mut writer = WriteSink(Vec::new());
            encode(&mut writer, preimage, LIMITS).expect("encodes");
            assert_eq!(writer.0, canonical, "{name}: io::Write sink");
        }
    }
}

/// PLAT-987 AC-2: the ordering vector really distinguishes the two orders —
/// sorting its names by code point gives a different sequence than its
/// expected canonical text.
#[test]
fn ordering_vector_is_order_sensitive() {
    let file = vectors_file();
    let vector = section(&file, "vectors")
        .iter()
        .find(|vector| field(vector, "name") == "utf16-order-differs-from-code-point-order")
        .expect("vector present");
    let object = vector
        .get("preimage")
        .and_then(Value::as_object)
        .expect("object preimage");
    let mut code_point: Vec<&str> = object.keys().map(String::as_str).collect();
    code_point.sort_unstable();
    let canonical = field(vector, "canonical");
    let positions: Vec<usize> = code_point
        .iter()
        .map(|name| {
            canonical
                .find(&format!("\"{name}\""))
                .expect("name present")
        })
        .collect();
    assert!(
        positions.windows(2).any(|pair| pair[0] > pair[1]),
        "code-point order must disagree with the canonical order"
    );
}

/// PLAT-987 AC-2: IEEE 754 doubles at the ECMAScript formatting boundaries
/// (RFC 8785 section 3.2.2.3), bit pattern to number text.
#[test]
fn ecmascript_number_formatting_boundaries() {
    let file = vectors_file();
    for number in section(&file, "numbers") {
        let name = field(number, "name");
        let value = double(field(number, "ieee754"));
        let bytes = to_vec(&value, LIMITS).unwrap_or_else(|error| panic!("{name}: {error}"));
        assert_eq!(text(&bytes), field(number, "canonical"), "{name}");
    }
}

/// RFC 8785 section 3.2.2.3: NaN and the infinities have no encoding.
#[test]
fn non_finite_numbers_are_refused() {
    let file = vectors_file();
    for number in section(&file, "refused_numbers") {
        let name = field(number, "name");
        let value = double(field(number, "ieee754"));
        assert!(
            matches!(to_vec(&value, LIMITS), Err(Error::NonFiniteNumber(_))),
            "{name}"
        );
    }
}

/// PLAT-987 AC-1, AC-3: typed values, with no `serde_json::Value` anywhere,
/// give the vector's bytes whatever order the input map iterates in.
#[test]
fn typed_maps_encode_in_utf16_order_regardless_of_backing() {
    let expected = "{\"~\":\"ascii\",\"\u{10000}\":\"linear b syllable\",\
                    \"\u{E000}\":\"private use\",\"\u{FFFD}\":\"replacement character\"}";
    let entries = [
        ("\u{FFFD}", "replacement character"),
        ("\u{E000}", "private use"),
        ("\u{10000}", "linear b syllable"),
        ("~", "ascii"),
    ];
    // BTreeMap iterates in scalar-value order, HashMap in arbitrary order, the
    // slice-of-pairs map in declaration order.
    let btree: BTreeMap<&str, &str> = entries.into_iter().collect();
    assert_eq!(text(&to_vec(&btree, LIMITS).expect("btree")), expected);
    // `HashMap` is a `FixedShape` only with the `std` feature.
    #[cfg(feature = "std")]
    {
        let hash: std::collections::HashMap<&str, &str> = entries.into_iter().collect();
        assert_eq!(text(&to_vec(&hash, LIMITS).expect("hash")), expected);
    }
    assert_eq!(
        text(&to_vec(&Pairs(&entries), LIMITS).expect("pairs")),
        expected
    );
}

/// The lane this binary was built for is the lane it claims to be: without
/// `test-preserve-order` `serde_json::Map` sorts its keys, with it the keys
/// keep insertion order. Without this, a Makefile edit that stopped passing
/// the feature would leave the second run silently identical to the first.
#[test]
fn serde_json_map_order_matches_the_lane() {
    let value: Value = serde_json::from_str(r#"{"b":1,"a":2}"#).expect("json");
    let keys: Vec<&str> = value
        .as_object()
        .expect("object")
        .keys()
        .map(String::as_str)
        .collect();
    if cfg!(feature = "test-preserve-order") {
        assert_eq!(
            keys,
            ["b", "a"],
            "preserve_order lane must keep insertion order"
        );
    } else {
        assert_eq!(keys, ["a", "b"], "default lane must sort keys");
    }
}

/// A map serialized in exactly the order its pairs are listed.
struct Pairs<'a>(&'a [(&'a str, &'a str)]);

impl FixedShape for Pairs<'_> {
    const DEPTH: usize = nest(&[<&str>::DEPTH]);
}

impl Serialize for Pairs<'_> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_map(self.0.iter().copied())
    }
}

fn double(bits: &str) -> f64 {
    f64::from_bits(u64::from_str_radix(bits, 16).expect("hex bit pattern"))
}

fn sha2_hasher() -> sha2::Sha256 {
    <sha2::Sha256 as sha2::Digest>::new()
}

fn finalize(hasher: sha2::Sha256) -> Vec<u8> {
    sha2::Digest::finalize(hasher).to_vec()
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}
