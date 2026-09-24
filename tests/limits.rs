// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Agent-IX
//! PLAT-987 AC-4: at a limit the encoder refuses with a cause naming the limit
//! kind and bound, and never returns truncated output as success.

use quire_canonical::{encode, sha256, to_vec, Error, LimitExceeded, LimitKind, Limits};
use serde::ser::{SerializeSeq, Serializer};
use serde::Serialize;

const DEPTH: u32 = 64;

#[derive(Serialize)]
struct Record {
    zeta: Vec<u32>,
    alpha: &'static str,
    nested: Inner,
}

#[derive(Serialize)]
struct Inner {
    y: bool,
    x: Option<u8>,
}

fn record() -> Record {
    Record {
        zeta: vec![1, 2, 3],
        alpha: "\u{00e9}t\u{00e9}",
        nested: Inner { y: true, x: None },
    }
}

const RECORD_CANONICAL: &str =
    "{\"alpha\":\"\u{00e9}t\u{00e9}\",\"nested\":{\"x\":null,\"y\":true},\"zeta\":[1,2,3]}";

#[test]
fn exact_bound_succeeds() {
    let length = u64::try_from(RECORD_CANONICAL.len()).expect("fits");
    let bytes = to_vec(&record(), Limits::new(length, DEPTH)).expect("fits exactly");
    assert_eq!(bytes, RECORD_CANONICAL.as_bytes());
}

#[test]
fn every_smaller_bound_refuses_with_the_byte_limit_and_never_truncates() {
    let length = u64::try_from(RECORD_CANONICAL.len()).expect("fits");
    for bound in 0..length {
        let limits = Limits::new(bound, DEPTH);
        match to_vec(&record(), limits) {
            Err(Error::Limit(LimitExceeded {
                kind: LimitKind::CanonicalBytes,
                bound: reported,
                required,
            })) => {
                assert_eq!(reported, bound);
                assert!(required > bound && required <= length, "bound {bound}");
            }
            other => panic!("bound {bound}: expected byte-limit refusal, got {other:?}"),
        }
        assert!(
            matches!(sha256(&record(), limits), Err(Error::Limit(_))),
            "no digest at bound {bound}"
        );
    }
}

/// The member buffers held for sorting count against the same ceiling: a
/// top-level object's members are all buffered until it closes, so a refusal
/// inside it reaches the sink with only the opening brace.
#[test]
fn sort_buffers_are_metered_before_they_reach_the_sink() {
    // 20 bytes is reached while the members are still being buffered.
    let mut sink = Vec::new();
    let refused = encode(&mut sink, &record(), Limits::new(20, DEPTH));
    assert!(matches!(refused, Err(Error::Limit(_))));
    assert_eq!(sink, b"{");
}

#[test]
fn limit_display_names_kind_and_bound() {
    let error = to_vec(&record(), Limits::new(10, DEPTH)).expect_err("too small");
    let message = error.to_string();
    assert!(message.contains("canonical bytes"), "{message}");
    assert!(message.contains("bound 10"), "{message}");
}

/// Arrays nested `levels` deep, built without recursion in the value.
struct Nest(u32);

impl Serialize for Nest {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut seq = serializer.serialize_seq(Some(1))?;
        if self.0 > 1 {
            seq.serialize_element(&Nest(self.0 - 1))?;
        }
        seq.end()
    }
}

#[test]
fn depth_at_bound_succeeds_and_beyond_refuses() {
    let bytes = to_vec(&Nest(4), Limits::new(1024, 4)).expect("depth 4 fits");
    assert_eq!(bytes, b"[[[[]]]]");
    match to_vec(&Nest(5), Limits::new(1024, 4)) {
        Err(Error::Limit(LimitExceeded {
            kind: LimitKind::NestingDepth,
            bound: 4,
            required: 5,
        })) => {}
        other => panic!("expected depth refusal, got {other:?}"),
    }
}

/// A hostile depth is refused at the bound, long before the native stack is
/// at risk.
#[test]
fn hostile_depth_is_refused_not_a_stack_overflow() {
    let refused = to_vec(&Nest(1_000_000), Limits::new(u64::MAX, DEPTH));
    assert!(matches!(
        refused,
        Err(Error::Limit(LimitExceeded {
            kind: LimitKind::NestingDepth,
            ..
        }))
    ));
}

#[test]
fn objects_and_enum_wrappers_count_toward_depth() {
    #[derive(Serialize)]
    enum Wrapper {
        Struct { inner: Inner },
    }
    let value = Wrapper::Struct {
        inner: Inner {
            y: false,
            x: Some(1),
        },
    };
    // `{"Struct":{"inner":{...}}}` opens three objects.
    assert!(to_vec(&value, Limits::new(1024, 3)).is_ok());
    assert!(matches!(
        to_vec(&value, Limits::new(1024, 2)),
        Err(Error::Limit(LimitExceeded {
            kind: LimitKind::NestingDepth,
            bound: 2,
            required: 3,
        }))
    ));
}
