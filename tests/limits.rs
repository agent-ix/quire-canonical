// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Agent-IX
//! PLAT-987 AC-4: at a limit the encoder refuses with a cause naming the limit
//! kind and bound, and never returns truncated output as success.
//!
//! The byte limits are the only limits (QSL FR-259, ADR-030 D-4.5), and every
//! heap stack the crate grows is bounded by one of them.

use quire_canonical::{
    encode, nest, read, sha256, to_vec, Error, FixedShape, LimitExceeded, LimitKind, Limits,
    Malformed, ReadError, Writer,
};
use serde::Serialize;

fn limits(max_bytes: u64) -> Limits {
    Limits::new(max_bytes)
}

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

impl FixedShape for Record {
    const DEPTH: usize = nest(&[Vec::<u32>::DEPTH, <&str>::DEPTH, Inner::DEPTH]);
}

impl FixedShape for Inner {
    const DEPTH: usize = nest(&[bool::DEPTH, Option::<u8>::DEPTH]);
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
    let bytes = to_vec(&record(), limits(length)).expect("fits exactly");
    assert_eq!(bytes, RECORD_CANONICAL.as_bytes());
}

#[test]
fn every_smaller_bound_refuses_with_the_byte_limit_and_never_truncates() {
    let length = u64::try_from(RECORD_CANONICAL.len()).expect("fits");
    for bound in 0..length {
        let limits = limits(bound);
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
    let refused = encode(&mut sink, &record(), limits(20));
    assert!(matches!(refused, Err(Error::Limit(_))));
    assert_eq!(sink, b"{");
}

#[test]
fn limit_display_names_kind_and_bound() {
    let error = to_vec(&record(), limits(10)).expect_err("too small");
    let message = error.to_string();
    assert!(message.contains("canonical bytes"), "{message}");
    assert!(message.contains("bound 10"), "{message}");
}

/// The writer's stack of open containers grows only by producing a `[` or
/// `{` first, so the byte ceiling bounds it. Every open costs one byte, so
/// `N` opens fit a ceiling of `N` bytes and the next is refused with the byte
/// limit, naming the bound, whatever the depth.
#[test]
fn deep_opens_are_refused_on_bytes_not_depth() {
    const CEILING: u64 = 10_000;
    let mut sink = Vec::new();
    let mut writer = Writer::new(&mut sink, limits(CEILING));
    for _ in 0..CEILING {
        writer.begin_array().expect("within the ceiling");
    }
    assert_eq!(writer.produced(), CEILING);
    match writer.begin_array() {
        Err(Error::Limit(LimitExceeded {
            kind: LimitKind::CanonicalBytes,
            bound: CEILING,
            required,
        })) => assert_eq!(required, CEILING + 1),
        other => panic!("expected the byte limit, got {other:?}"),
    }
    assert_eq!(sink.len(), usize::try_from(CEILING).expect("fits"));
}

/// Objects nested inside objects are buffered for sorting; their records and
/// the buffer are bounded by the same ceiling.
#[test]
fn nested_object_buffers_are_refused_on_bytes() {
    let mut sink = Vec::new();
    let mut writer = Writer::new(&mut sink, limits(64));
    let refused = (0..64).try_for_each(|_| {
        writer.begin_object()?;
        writer.name("k")
    });
    assert!(matches!(
        refused,
        Err(Error::Limit(LimitExceeded {
            kind: LimitKind::CanonicalBytes,
            bound: 64,
            ..
        }))
    ));
    assert_eq!(sink, b"{", "nothing past the outer brace reached the sink");
}

/// The tree encoder's task stack is bounded by the document's nodes, and the
/// output it drives is bounded by the byte ceiling: a deep document under a
/// small ceiling is refused on bytes.
#[test]
fn deep_document_encoding_is_refused_on_bytes() {
    let text = format!("{}{}", "[".repeat(5_000), "]".repeat(5_000));
    let document = read(text.as_bytes(), u64::MAX).expect("reads");
    assert!(matches!(
        to_vec(&document, limits(4_999)),
        Err(Error::Limit(LimitExceeded {
            kind: LimitKind::CanonicalBytes,
            bound: 4_999,
            required: 5_000,
        }))
    ));
    assert_eq!(
        to_vec(&document, limits(10_000)).expect("fits").len(),
        10_000
    );
}

/// The reader's stack, nodes and text are linear in the input, and the input
/// byte limit is checked before any of it is read: an over-limit input is a
/// byte refusal even when it is also malformed, never a malformed-input one.
#[test]
fn reader_input_byte_limit_is_checked_first_and_is_not_malformed_input() {
    let deep = "[".repeat(1_001);
    match read(deep.as_bytes(), 1_000) {
        Err(ReadError::Limit(LimitExceeded {
            kind: LimitKind::InputBytes,
            bound: 1_000,
            required: 1_001,
        })) => {}
        other => panic!("expected the input byte limit, got {other:?}"),
    }
    // At the limit the same text is read, and refused on its content.
    assert!(matches!(
        read(&deep.as_bytes()[..1_000], 1_000),
        Err(ReadError::Malformed {
            offset: 1_000,
            kind: Malformed::UnexpectedEnd,
        })
    ));
}
