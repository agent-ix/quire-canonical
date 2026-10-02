// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Agent-IX
//! The shared reader of untrusted JSON (QSL FR-259, ADR-030 D-4.5): it reads
//! into an arena tree, refuses malformed input with the byte offset of the
//! fault, and the tree encodes to RFC 8785 text.

use quire_canonical::{read, to_vec, Limits, Malformed, Node, ReadError};

const LIMITS: Limits = Limits::new(1 << 20);

fn refusal(text: &str) -> (usize, Malformed) {
    match read(text.as_bytes(), u64::MAX) {
        Err(ReadError::Malformed { offset, kind }) => (offset, kind),
        other => panic!("{text:?}: expected a malformed-input refusal, got {other:?}"),
    }
}

fn canonical(text: &str) -> String {
    let document = read(text.as_bytes(), u64::MAX).expect("reads");
    String::from_utf8(to_vec(&document, LIMITS).expect("encodes")).expect("UTF-8")
}

/// FR-259: a lone surrogate escape is refused at the offset of its `\`.
#[test]
fn lone_surrogate_escape_is_refused_at_its_offset() {
    assert_eq!(
        refusal(r#"{"digest": "\ud800"}"#),
        (12, Malformed::LoneSurrogate)
    );
    assert_eq!(refusal(r#"["\udc00"]"#), (2, Malformed::LoneSurrogate));
    // A high half followed by something other than a low half.
    assert_eq!(refusal(r#""a\ud800\u0041""#), (2, Malformed::LoneSurrogate));
    assert_eq!(refusal(r#""\ud800x""#), (1, Malformed::LoneSurrogate));
}

/// A surrogate pair decodes to its one character.
#[test]
fn surrogate_pair_decodes() {
    assert_eq!(canonical(r#""\ud834\udd1e""#), "\"\u{1d11e}\"");
}

/// FR-259: a number with no finite double is refused at its first byte.
#[test]
fn number_out_of_double_range_is_refused_at_its_offset() {
    assert_eq!(refusal(r#"{"n": 1e400}"#), (6, Malformed::NumberOutOfRange));
    assert_eq!(refusal("[-1e400]"), (1, Malformed::NumberOutOfRange));
}

/// Every other departure from the grammar is refused where it starts.
#[test]
fn malformed_input_is_refused_with_its_offset() {
    for (text, expected) in [
        ("", (0, Malformed::UnexpectedEnd)),
        ("   ", (3, Malformed::UnexpectedEnd)),
        ("[1,]", (3, Malformed::UnexpectedCharacter)),
        ("[1 2]", (3, Malformed::UnexpectedCharacter)),
        (r#"{"a" 1}"#, (5, Malformed::UnexpectedCharacter)),
        (r#"{"a":1,}"#, (7, Malformed::UnexpectedCharacter)),
        ("{1:2}", (1, Malformed::UnexpectedCharacter)),
        ("[}", (1, Malformed::UnexpectedCharacter)),
        ("[1", (2, Malformed::UnexpectedEnd)),
        ("tru", (0, Malformed::UnexpectedCharacter)),
        ("01", (1, Malformed::TrailingContent)),
        ("1.", (0, Malformed::InvalidNumber)),
        ("-", (0, Malformed::InvalidNumber)),
        ("1e+", (0, Malformed::InvalidNumber)),
        (".5", (0, Malformed::UnexpectedCharacter)),
        ("\"a\u{1}\"", (2, Malformed::ControlCharacter)),
        (r#""\x""#, (1, Malformed::InvalidEscape)),
        (r#""\u12g4""#, (1, Malformed::InvalidEscape)),
        ("\"abc", (4, Malformed::UnexpectedEnd)),
        ("null null", (5, Malformed::TrailingContent)),
        ("\u{feff}null", (0, Malformed::UnexpectedCharacter)),
        (r#"{"a":1,"b":2,"a":3}"#, (13, Malformed::DuplicateName)),
        (r#"{"\u0061":1,"a":2}"#, (12, Malformed::DuplicateName)),
    ] {
        assert_eq!(refusal(text), expected, "{text:?}");
    }
    match read(b"[\"\xff\"]", u64::MAX) {
        Err(ReadError::Malformed {
            offset: 2,
            kind: Malformed::InvalidUtf8,
        }) => {}
        other => panic!("expected invalid UTF-8 at 2, got {other:?}"),
    }
}

/// The tree holds each value: decoded strings, numbers with their source
/// text and double, and members in document order.
#[test]
fn tree_holds_values_numbers_keep_their_text() {
    let document = read(
        br#" {"b": [true, null, "x\ny"], "a": 1.50, "c": 18446744073709551617} "#,
        u64::MAX,
    )
    .expect("reads");
    let root = document.root();
    let Node::Object(members) = root.node() else {
        panic!("object root");
    };
    let names: Vec<&str> = members.map(|(name, _)| name).collect();
    assert_eq!(names, ["b", "a", "c"]);

    let Some(Node::Number(number)) = root.get("a").map(|node| node.node()) else {
        panic!("number member");
    };
    assert_eq!((number.text(), number.value()), ("1.50", 1.5));
    let Some(Node::Number(big)) = root.get("c").map(|node| node.node()) else {
        panic!("number member");
    };
    assert_eq!(big.text(), "18446744073709551617");

    let Some(Node::Array(items)) = root.get("b").map(|node| node.node()) else {
        panic!("array member");
    };
    let items: Vec<String> = items.map(|item| format!("{:?}", item.node())).collect();
    assert_eq!(items, ["Bool(true)", "Null", "String(\"x\\ny\")"]);
    assert!(root.get("missing").is_none());
}

/// The tree encodes to RFC 8785 text: members sorted by UTF-16 code unit,
/// whitespace dropped, strings re-escaped, numbers spelled from their double.
#[test]
fn tree_encodes_to_canonical_text() {
    assert_eq!(
        canonical(
            "{ \"\u{fffd}\": 1, \"\u{10000}\": 2, \"~\": [ 1E2, -0, 0.000001, \"\\u0041\\/\" ] }"
        ),
        "{\"~\":[100,0,0.000001,\"A/\"],\"\u{10000}\":2,\"\u{fffd}\":1}"
    );
    assert_eq!(canonical(" [ ] "), "[]");
    assert_eq!(canonical("{}"), "{}");
    assert_eq!(canonical("\"\\u001f\""), "\"\\u001f\"");
}

/// A JSON number is the double its text denotes (RFC 8785 §3.2.2.3), so an
/// integer literal past `2^53` encodes as its nearest double, the way
/// ECMAScript reads it. Only a Rust integer past `2^53` is refused.
#[test]
fn json_text_integers_encode_as_their_double() {
    for (text, expected) in [
        ("9007199254740993", "9007199254740992"),
        ("-9007199254740993", "-9007199254740992"),
        ("18014398509481984", "18014398509481984"),
        ("9223372036854775807", "9223372036854776000"),
        ("-9223372036854775808", "-9223372036854776000"),
        ("18446744073709551617", "18446744073709552000"),
        ("100000000000000000001", "100000000000000000000"),
    ] {
        assert_eq!(canonical(text), expected, "{text}");
    }
}

/// A subtree encodes on its own.
#[test]
fn a_subtree_encodes_on_its_own() {
    let document = read(br#"{"wire": {"y": [2, 1], "x": {}}, "z": 0}"#, u64::MAX).expect("reads");
    let wire = document.root().get("wire").expect("member");
    assert_eq!(
        to_vec(&wire, LIMITS).expect("encodes"),
        br#"{"x":{},"y":[2,1]}"#
    );
}
