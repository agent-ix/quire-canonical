// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Agent-IX
//! `#[derive(FixedShape)]`: the serde path only fixed-depth types can take
//! (QSL FR-259, ADR-030 D-4.5).

use std::collections::BTreeMap;
use std::path::Path;
use std::process::Command;

use quire_canonical::{to_vec, FixedShape, Limits};
use serde::Serialize;

const LIMITS: Limits = Limits::new(1 << 10);

#[derive(Serialize, FixedShape)]
struct Unit;

#[derive(Serialize, FixedShape)]
struct Newtype(u8);

#[derive(Serialize, FixedShape)]
struct Pair(u8, Vec<u8>);

#[derive(Serialize, FixedShape)]
struct Record {
    name: String,
    tags: Vec<String>,
    inner: Option<Pair>,
}

#[derive(Serialize, FixedShape)]
enum Choice {
    Plain,
    Wrapped(u8),
    Tuple(u8, u8),
    Fields { list: Vec<BTreeMap<String, u8>> },
}

#[derive(Serialize, FixedShape)]
struct Generic<T> {
    value: T,
}

/// The derived `DEPTH` follows serde's default representation: newtypes are
/// transparent, structs and tuples add one level, enum variants add their
/// `{"variant":...}` wrapper, and an enum is its deepest variant.
#[test]
fn derived_depth_follows_the_serde_shape() {
    assert_eq!(Unit::DEPTH, 0);
    assert_eq!(Newtype::DEPTH, 0);
    assert_eq!(Pair::DEPTH, 2);
    assert_eq!(Record::DEPTH, 3);
    assert_eq!(Choice::DEPTH, 4);
    assert_eq!(Generic::<u8>::DEPTH, 1);
    assert_eq!(Generic::<Record>::DEPTH, 4);
}

/// Derived types take the serde path and encode canonically.
#[test]
fn derived_types_encode() {
    let record = Record {
        name: "n".to_owned(),
        tags: vec!["t".to_owned()],
        inner: Some(Pair(1, vec![2])),
    };
    assert_eq!(
        to_vec(&record, LIMITS).expect("encodes"),
        br#"{"inner":[1,[2]],"name":"n","tags":["t"]}"#
    );
    for (choice, expected) in [
        (Choice::Plain, &br#""Plain""#[..]),
        (Choice::Wrapped(1), br#"{"Wrapped":1}"#),
        (Choice::Tuple(1, 2), br#"{"Tuple":[1,2]}"#),
    ] {
        assert_eq!(to_vec(&choice, LIMITS).expect("encodes"), expected);
    }
    let choice = Choice::Fields {
        list: vec![BTreeMap::from([("k".to_owned(), 1)])],
    };
    assert_eq!(
        to_vec(&choice, LIMITS).expect("encodes"),
        br#"{"Fields":{"list":[{"k":1}]}}"#
    );
    assert_eq!(
        to_vec(&Generic { value: Newtype(3) }, LIMITS).expect("encodes"),
        br#"{"value":3}"#
    );
}

/// A recursive type that derives `FixedShape` does not compile: its `DEPTH`
/// refers to itself and rustc refuses the cycle with E0391. The fixture crate
/// is built with the same cargo, against this crate by path.
#[test]
fn recursive_type_deriving_fixed_shape_fails_with_e0391() {
    let fixture = Path::new(env!("CARGO_TARGET_TMPDIR")).join("derive-recursive");
    let source = fixture.join("src");
    std::fs::create_dir_all(&source).expect("fixture directory");
    let manifest = format!(
        r#"[package]
name = "derive-recursive"
version = "0.0.0"
edition = "2021"
publish = false

[workspace]

[dependencies]
quire-canonical = {{ path = {crate_dir:?}, default-features = false }}
serde = {{ version = "1", default-features = false, features = ["derive", "alloc"] }}
"#,
        crate_dir = env!("CARGO_MANIFEST_DIR"),
    );
    std::fs::write(fixture.join("Cargo.toml"), manifest).expect("manifest");
    std::fs::write(
        source.join("lib.rs"),
        r#"
use quire_canonical::{to_vec, FixedShape, Limits};

#[derive(serde::Serialize, FixedShape)]
pub struct List {
    next: Option<Box<List>>,
}

pub fn encode() -> usize {
    to_vec(&List { next: None }, Limits::new(64)).map_or(0, |bytes| bytes.len())
}
"#,
    )
    .expect("source");
    let output = Command::new(env!("CARGO"))
        .arg("build")
        .arg("--offline")
        .arg("--quiet")
        .arg("--manifest-path")
        .arg(fixture.join("Cargo.toml"))
        .env("CARGO_TARGET_DIR", fixture.join("target"))
        .output()
        .expect("cargo runs");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success(), "a recursive type compiled");
    assert!(
        stderr.contains("error[E0391]"),
        "expected a cycle error (E0391), got:\n{stderr}"
    );
}
