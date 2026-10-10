// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Agent-IX
//! Verified root ordering through the public writer API.

use std::cell::RefCell;

use quire_canonical::{Error, LimitKind, Limits, ProtocolViolation, Sink, Writer};
use sha2::{Digest, Sha256};

const LIMITS: Limits = Limits::new(1 << 20);
const NAMES: [&str; 8] = ["", "\n", "\"", "a", "aa", "\u{d7ff}", "😀", "\u{e000}"];
const TEXT: &str = "{\"\":0,\"\\n\":1,\"\\\"\":2,\"a\":3,\"aa\":4,\"퟿\":5,\"😀\":6,\"\":7}";

fn members<S: Sink>(writer: &mut Writer<'_, S>) {
    for (index, name) in NAMES.iter().enumerate() {
        writer.name(name).unwrap();
        writer.integer(index as i128).unwrap();
    }
}

#[test]
fn literal_utf16_bytes_match_buffered_path_and_exact_byte_hash() {
    let mut both = (Vec::new(), Sha256::new());
    let mut writer = Writer::new(&mut both, LIMITS);
    writer.begin_ordered_object().unwrap();
    members(&mut writer);
    writer.end_object().unwrap();
    assert_eq!(writer.finish().unwrap(), TEXT.len() as u64);
    assert_eq!(both.0, TEXT.as_bytes());
    assert_eq!(both.1.finalize(), Sha256::digest(TEXT.as_bytes()));

    let mut buffered = Vec::new();
    let mut writer = Writer::new(&mut buffered, LIMITS);
    writer.begin_object().unwrap();
    for (index, name) in NAMES.iter().enumerate().rev() {
        writer.name(name).unwrap();
        writer.integer(index as i128).unwrap();
    }
    writer.end_object().unwrap();
    writer.finish().unwrap();
    assert_eq!(buffered, TEXT.as_bytes());
}

#[test]
fn exact_byte_boundary_succeeds_and_one_less_refuses_closure() {
    for bound in [TEXT.len() as u64, TEXT.len() as u64 - 1] {
        let mut bytes = Vec::new();
        let mut writer = Writer::new(&mut bytes, Limits::new(bound));
        writer.begin_ordered_object().unwrap();
        members(&mut writer);
        if bound == TEXT.len() as u64 {
            writer.end_object().unwrap();
            assert_eq!(writer.finish().unwrap(), bound);
            assert_eq!(bytes, TEXT.as_bytes());
        } else {
            assert!(matches!(writer.end_object(), Err(Error::Limit(limit))
                if limit.kind == LimitKind::CanonicalBytes && limit.bound == bound
                && limit.required == bound + 1));
            assert!(matches!(writer.finish(), Err(Error::Protocol(ProtocolViolation::AfterRefusal))));
        }
    }
}

#[test]
fn duplicates_and_descending_names_refuse_precisely_and_poison() {
    for (first, next, duplicate) in [("a", "a", true), ("aa", "a", false), ("\u{e000}", "😀", false), ("\n", "\n", true)] {
        let mut bytes = Vec::new();
        let mut writer = Writer::new(&mut bytes, LIMITS);
        writer.begin_ordered_object().unwrap();
        writer.name(first).unwrap();
        writer.null().unwrap();
        let error = writer.name(next).unwrap_err();
        if duplicate {
            assert!(matches!(error, Error::DuplicateMemberName { name } if name == next));
        } else {
            assert!(matches!(error, Error::MemberNameOutOfOrder));
        }
        assert!(matches!(writer.null(), Err(Error::Protocol(ProtocolViolation::AfterRefusal))));
        assert!(matches!(writer.end_object(), Err(Error::Protocol(ProtocolViolation::AfterRefusal))));
        assert!(matches!(writer.finish(), Err(Error::Protocol(ProtocolViolation::AfterRefusal))));
    }
}

struct Observe<'a>(&'a RefCell<Vec<u8>>);
impl Sink for Observe<'_> {
    fn write_bytes(&mut self, bytes: &[u8]) -> Result<(), Error> {
        self.0.borrow_mut().extend_from_slice(bytes);
        Ok(())
    }
}

#[test]
fn output_is_incremental_and_nested_objects_still_sort() {
    let seen = RefCell::new(Vec::new());
    let mut sink = Observe(&seen);
    let mut writer = Writer::new(&mut sink, LIMITS);
    writer.begin_ordered_object().unwrap();
    assert_eq!(*seen.borrow(), b"{");
    writer.name("a").unwrap();
    writer.begin_array().unwrap();
    writer.string("\u{0}\n\"\\").unwrap();
    assert_eq!(*seen.borrow(), br#"{"a":["\u0000\n\"\\""#);
    writer.begin_object().unwrap();
    writer.name("z").unwrap();
    writer.integer(2).unwrap();
    writer.name("a").unwrap();
    writer.number(-0.0).unwrap();
    writer.end_object().unwrap();
    writer.end_array().unwrap();
    assert_eq!(*seen.borrow(), br#"{"a":["\u0000\n\"\\",{"a":0,"z":2}]"#);
    writer.name("b").unwrap();
    writer.serialize(&true).unwrap();
    writer.end_object().unwrap();
    let length = writer.finish().unwrap();
    assert_eq!(*seen.borrow(), br#"{"a":["\u0000\n\"\\",{"a":0,"z":2}],"b":true}"#);
    assert_eq!(length, seen.borrow().len() as u64);
}

#[test]
fn empty_root_and_existing_scalar_paths_remain_valid() {
    let mut bytes = Vec::new();
    let mut writer = Writer::new(&mut bytes, Limits::new(2));
    writer.begin_ordered_object().unwrap();
    writer.end_object().unwrap();
    assert_eq!(writer.finish().unwrap(), 2);
    assert_eq!(bytes, b"{}");
    bytes.clear();
    let mut writer = Writer::new(&mut bytes, Limits::new(4));
    writer.null().unwrap();
    assert_eq!(writer.finish().unwrap(), 4);
    assert_eq!(bytes, b"null");
}

#[test]
fn protocol_and_unclosed_container_refusals() {
    for case in 0..8 {
        let mut sink = Vec::new();
        let mut writer = Writer::new(&mut sink, LIMITS);
        writer.begin_ordered_object().unwrap();
        let (result, expected) = match case {
            0 => (writer.null(), ProtocolViolation::ValueWithoutName),
            1 => (writer.end_array(), ProtocolViolation::MismatchedEnd),
            2 => { writer.name("a").unwrap(); (writer.name("b"), ProtocolViolation::NameWithoutValue) }
            3 => { writer.name("a").unwrap(); (writer.end_object(), ProtocolViolation::ObjectEndedAfterName) }
            4 => { writer.name("a").unwrap(); (writer.begin_ordered_object(), ProtocolViolation::OrderedObjectNotRoot) }
            5 => { writer.end_object().unwrap(); (writer.null(), ProtocolViolation::SecondValue) }
            6 => { writer.end_object().unwrap(); (writer.name("a"), ProtocolViolation::NameOutsideObject) }
            _ => { writer.name("a").unwrap(); writer.begin_array().unwrap(); (writer.end_object(), ProtocolViolation::MismatchedEnd) }
        };
        assert!(matches!(result, Err(Error::Protocol(actual)) if actual == expected));
        assert!(matches!(writer.finish(), Err(Error::Protocol(ProtocolViolation::AfterRefusal))));
    }
    for nested in [false, true] {
        let mut sink = Vec::new();
        let mut writer = Writer::new(&mut sink, LIMITS);
        writer.begin_ordered_object().unwrap();
        if nested { writer.name("a").unwrap(); writer.begin_object().unwrap(); }
        assert!(matches!(writer.finish(), Err(Error::Protocol(ProtocolViolation::Incomplete))));
    }
}

#[test]
fn nested_limits_duplicates_and_number_refusals_are_not_bypassed() {
    let mut bytes = Vec::new();
    let mut writer = Writer::new(&mut bytes, Limits::new(8));
    writer.begin_ordered_object().unwrap();
    writer.name("a").unwrap();
    writer.begin_array().unwrap();
    assert!(matches!(writer.null(), Err(Error::Limit(limit)) if limit.kind == LimitKind::CanonicalBytes));
    assert!(matches!(writer.finish(), Err(Error::Protocol(ProtocolViolation::AfterRefusal))));

    let mut writer = Writer::new(&mut bytes, LIMITS);
    writer.begin_ordered_object().unwrap();
    writer.name("a").unwrap();
    writer.begin_object().unwrap();
    for _ in 0..2 { writer.name("x").unwrap(); writer.null().unwrap(); }
    assert!(matches!(writer.end_object(), Err(Error::DuplicateMemberName { name }) if name == "x"));
    assert!(matches!(writer.finish(), Err(Error::Protocol(ProtocolViolation::AfterRefusal))));

    for integer in [false, true] {
        let mut writer = Writer::new(&mut bytes, LIMITS);
        writer.begin_ordered_object().unwrap();
        writer.name("a").unwrap();
        if integer {
            assert!(matches!(writer.integer(9007199254740993), Err(Error::IntegerMagnitudeAboveMaximum(9007199254740993))));
        } else {
            assert!(matches!(writer.number(f64::INFINITY), Err(Error::NonFiniteNumber(_))));
        }
        assert!(matches!(writer.finish(), Err(Error::Protocol(ProtocolViolation::AfterRefusal))));
    }
}

#[cfg(feature = "std")]
#[test]
fn sink_failure_cannot_finish_successfully() {
    struct Fail;
    impl Sink for Fail {
        fn write_bytes(&mut self, _: &[u8]) -> Result<(), Error> {
            Err(Error::Sink(std::io::Error::other("sink refused")))
        }
    }
    let mut sink = (Sha256::new(), Fail);
    let mut writer = Writer::new(&mut sink, LIMITS);
    assert!(matches!(writer.begin_ordered_object(), Err(Error::Sink(_))));
    assert!(matches!(writer.finish(), Err(Error::Protocol(ProtocolViolation::AfterRefusal))));
}

#[test]
fn deep_nested_values_use_the_existing_heap_stack() {
    std::thread::Builder::new().stack_size(512 * 1024).spawn(|| {
        let mut sink = Sha256::new();
        let mut writer = Writer::new(&mut sink, LIMITS);
        writer.begin_ordered_object().unwrap();
        writer.name("a").unwrap();
        for _ in 0..100_000 { writer.begin_array().unwrap(); }
        writer.null().unwrap();
        for _ in 0..100_000 { writer.end_array().unwrap(); }
        writer.end_object().unwrap();
        assert_eq!(writer.finish().unwrap(), 200_010);
    }).unwrap().join().unwrap();
}
