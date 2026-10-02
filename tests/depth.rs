// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Agent-IX
//! Depth is never a limit (QSL FR-259, ADR-030 D-4.5): the event API, the
//! reader, the arena tree and the tree encoder handle 100,000 levels of
//! nesting on a 512 KiB thread stack, where any walk that recursed once per
//! level would overflow, and give the same bytes as on an 8 MiB stack.

use quire_canonical::{encode, read, sha256, to_vec, Document, Limits, Sha256Digest, Writer};
use sha2::Digest as _;

const DEEP: usize = 100_000;
const SMALL_STACK: usize = 512 * 1024;
const LARGE_STACK: usize = 8 * 1024 * 1024;
const LIMITS: Limits = Limits::new(u64::MAX);

/// Run `work` on a thread with a `stack`-byte stack.
fn on_stack<T: Send + 'static>(stack: usize, work: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::Builder::new()
        .stack_size(stack)
        .spawn(work)
        .expect("spawn")
        .join()
        .expect("no stack overflow")
}

/// A recursive list value `DEEP` cells long, `{"head":n,"tail":{...}}`, pushed
/// as events from a loop: the shape of a recursive list value keyed as
/// simulation state.
fn list_value_bytes() -> Vec<u8> {
    let mut bytes = Vec::new();
    let mut writer = Writer::new(&mut bytes, LIMITS);
    for _ in 0..DEEP {
        writer.begin_object().expect("open cell");
        writer.name("tail").expect("tail name");
    }
    writer.null().expect("end of list");
    for index in (0..DEEP).rev() {
        writer.name("head").expect("head name");
        writer.integer(index as i128).expect("head");
        writer.end_object().expect("close cell");
    }
    writer.finish().expect("one complete value");
    bytes
}

/// FR-259 (event API): a 100,000-long recursive value pushed from the caller's
/// loop encodes on a 512 KiB stack to the bytes it encodes to on an 8 MiB
/// stack, with every object's members sorted.
#[test]
fn event_api_encodes_a_100000_deep_value_on_a_small_stack() {
    let bytes = on_stack(SMALL_STACK, list_value_bytes);
    assert_eq!(bytes, on_stack(LARGE_STACK, list_value_bytes));
    let mut expected = String::new();
    for index in 0..DEEP {
        expected.push_str(&format!(r#"{{"head":{index},"tail":"#));
    }
    expected.push_str("null");
    expected.push_str(&"}".repeat(DEEP));
    assert_eq!(String::from_utf8(bytes).expect("UTF-8"), expected);
}

/// A 100,000-deep JSON package document: arrays and objects alternating,
/// with members out of canonical order at every level.
fn deep_document_text() -> String {
    let mut text = String::new();
    for _ in 0..DEEP / 2 {
        text.push_str(r#"{"z": 1.0, "a": ["#);
    }
    text.push_str("\"leaf\"");
    for _ in 0..DEEP / 2 {
        text.push_str("]}");
    }
    text
}

/// The same document's RFC 8785 text, built independently of the crate.
fn deep_document_canonical() -> String {
    let mut text = String::new();
    for _ in 0..DEEP / 2 {
        text.push_str(r#"{"a":["#);
    }
    text.push_str("\"leaf\"");
    for _ in 0..DEEP / 2 {
        text.push_str(r#"],"z":1}"#);
    }
    text
}

fn read_and_digest() -> (Sha256Digest, usize) {
    let text = deep_document_text();
    let document = read(text.as_bytes(), u64::MAX).expect("reads at any depth");
    let digest = sha256(&document, LIMITS).expect("digests at any depth");
    let length = to_vec(&document, LIMITS).expect("encodes").len();
    (digest, length)
}

/// FR-259 (reader and tree encoder): a 100,000-deep document is read and its
/// `sha256-jcs` digest taken on a 512 KiB stack; the digest equals the one
/// taken on an 8 MiB stack, and equals the SHA-256 of the document's RFC 8785
/// text.
#[test]
fn reader_and_tree_digest_a_100000_deep_document_on_a_small_stack() {
    let small = on_stack(SMALL_STACK, read_and_digest);
    let large = on_stack(LARGE_STACK, read_and_digest);
    assert_eq!(small, large);
    let canonical = deep_document_canonical();
    let expected: [u8; 32] = sha2::Sha256::digest(canonical.as_bytes()).into();
    assert_eq!(small.0.as_bytes(), &expected);
    assert_eq!(small.1, canonical.len());
}

/// FR-259 (a tree whose traits do not recurse): cloning, comparing, printing
/// and dropping a 100,000-deep document all run on a 512 KiB stack.
#[test]
fn deep_document_traits_do_not_recurse() {
    let outcome = on_stack(SMALL_STACK, || {
        let text = deep_document_text();
        let document = read(text.as_bytes(), u64::MAX).expect("reads");
        let copy: Document = document.clone();
        let equal = copy == document;
        let printed = !format!("{document:?}").is_empty();
        drop(copy);
        drop(document);
        (equal, printed)
    });
    assert_eq!(outcome, (true, true));
}

/// A deep array pushed as events streams to the sink as it is written:
/// nothing waits for the outer array to close.
#[test]
fn deep_arrays_stream_through_the_event_api() {
    let bytes = on_stack(SMALL_STACK, || {
        let mut sink = Vec::new();
        let mut writer = Writer::new(&mut sink, LIMITS);
        for _ in 0..DEEP {
            writer.begin_array().expect("open");
        }
        assert_eq!(writer.produced(), DEEP as u64);
        for _ in 0..DEEP {
            writer.end_array().expect("close");
        }
        assert_eq!(writer.finish().expect("complete"), 2 * DEEP as u64);
        sink
    });
    assert_eq!(bytes.len(), 2 * DEEP);
    assert!(bytes[..DEEP].iter().all(|byte| *byte == b'['));
    assert!(bytes[DEEP..].iter().all(|byte| *byte == b']'));
    // The same text read back and re-encoded is unchanged.
    let round_trip = on_stack(SMALL_STACK, move || {
        let document = read(&bytes, u64::MAX).expect("reads");
        let mut again = Vec::new();
        encode(&mut again, &document, LIMITS).expect("encodes");
        again == bytes
    });
    assert!(round_trip);
}
