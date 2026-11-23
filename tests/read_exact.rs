// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Agent-IX
//! `read_exact_with_reservation` against the growing reader: the same
//! outcome and document for every input, one exact reservation per list at
//! the size the formula in its docs gives, strictly fewer bytes asked than
//! the growing reader, a denial at any request that leaves nothing asked
//! after it, and the documented outcome order under a tight budget.
//!
//! The sizes in the formula are those of a 64-bit target.
#![cfg(target_pointer_width = "64")]

use proptest::prelude::*;
use quire_canonical::{
    read, read_exact_with_reservation, read_with_reservation, Document, LimitKind, Malformed, Node,
    NodeRef, ReadError, Reservation, ReservationDenied,
};

/// Records every request, in order, and denies the one at `deny`.
#[derive(Default)]
struct Recorder {
    deny: Option<usize>,
    requests: Vec<usize>,
}

impl Recorder {
    fn denying(index: usize) -> Self {
        Self {
            deny: Some(index),
            requests: Vec::new(),
        }
    }

    fn total(&self) -> usize {
        self.requests.iter().sum()
    }
}

impl Reservation for Recorder {
    fn reserve(&mut self, bytes: usize) -> Result<(), ReservationDenied> {
        let index = self.requests.len();
        self.requests.push(bytes);
        if self.deny == Some(index) {
            return Err(ReservationDenied);
        }
        Ok(())
    }
}

/// Admits up to `bound` bytes in total.
struct Budget {
    bound: usize,
    charged: usize,
}

impl Reservation for Budget {
    fn reserve(&mut self, bytes: usize) -> Result<(), ReservationDenied> {
        let total = self.charged.saturating_add(bytes);
        if total > self.bound {
            return Err(ReservationDenied);
        }
        self.charged = total;
        Ok(())
    }
}

const AMPLE: u64 = u64::MAX;

fn growing(input: &[u8]) -> (Result<Document, ReadError>, Recorder) {
    let mut recorder = Recorder::default();
    let outcome = read_with_reservation(input, AMPLE, &mut recorder);
    (outcome, recorder)
}

fn exact(input: &[u8]) -> (Result<Document, ReadError>, Recorder) {
    let mut recorder = Recorder::default();
    let outcome = read_exact_with_reservation(input, AMPLE, &mut recorder);
    (outcome, recorder)
}

/// What pass 1 counts, computed from the finished document.
#[derive(Debug, Default)]
struct Counts {
    text: usize,
    slots: usize,
    items: usize,
    members: usize,
    open_items: usize,
    open_members: usize,
    sort: usize,
    depth: usize,
}

/// The scratch high-water marks and depth of the subtree at `node`, adding
/// its text, nodes, items and members to `counts`. Test documents are
/// shallow, so this recurses.
fn walk(node: NodeRef<'_>, counts: &mut Counts) -> (usize, usize, usize) {
    counts.slots += 1;
    match node.node() {
        Node::Null | Node::Bool(_) => (0, 0, 0),
        Node::Number(number) => {
            counts.text += number.text().len();
            (0, 0, 0)
        }
        Node::String(text) => {
            counts.text += text.len();
            (0, 0, 0)
        }
        Node::Array(items) => {
            let width = items.len();
            counts.items += width;
            let (mut open_items, mut open_members, mut depth) = (width, 0, 0);
            for (index, child) in items.enumerate() {
                let (child_items, child_members, child_depth) = walk(child, counts);
                open_items = open_items.max(index + child_items);
                open_members = open_members.max(child_members);
                depth = depth.max(child_depth);
            }
            (open_items, open_members, depth + 1)
        }
        Node::Object(members) => {
            let width = members.len();
            counts.members += width;
            if width >= 2 {
                counts.sort = counts.sort.max(width);
            }
            let (mut open_items, mut open_members, mut depth) = (0, 0, 0);
            for (index, (name, child)) in members.enumerate() {
                counts.text += name.len();
                let (child_items, child_members, child_depth) = walk(child, counts);
                open_items = open_items.max(child_items);
                open_members = open_members.max(index + 1 + child_members);
                depth = depth.max(child_depth);
            }
            (open_items, open_members, depth + 1)
        }
    }
}

fn counts(document: &Document) -> Counts {
    let mut counts = Counts::default();
    let (open_items, open_members, depth) = walk(document.root(), &mut counts);
    counts.open_items = open_items;
    counts.open_members = open_members;
    counts.depth = depth;
    counts
}

/// The requests the docs promise for an accepted input, in order: the
/// stack's growth to the deepest nesting, then each list once.
fn promised_requests(counts: &Counts) -> Vec<usize> {
    let mut requests = Vec::new();
    let mut capacity = 0_usize;
    while capacity < counts.depth {
        capacity = (capacity * 2).max(4);
        requests.push(16 * capacity);
    }
    for bytes in [
        counts.text,
        32 * counts.slots,
        8 * counts.items,
        24 * counts.members,
        8 * counts.open_items,
        32 * counts.open_members,
        8 * counts.sort,
    ] {
        if bytes > 0 {
            requests.push(bytes);
        }
    }
    requests
}

/// The exact reader agrees with the growing one on `input`; for an accepted
/// input it asks exactly the promised requests, fewer bytes in total, and
/// keeps exactly the arena. A denial at any request is that request's
/// refusal, with nothing asked after it.
fn check(input: &[u8]) {
    let (old, old_recorder) = growing(input);
    let (new, new_recorder) = exact(input);
    assert_eq!(new, old, "{:?}", String::from_utf8_lossy(input));
    assert_eq!(read(input, AMPLE), old);
    if let Ok(document) = &new {
        let counts = counts(document);
        assert_eq!(
            new_recorder.requests,
            promised_requests(&counts),
            "{counts:?}"
        );
        assert!(
            new_recorder.total() < old_recorder.total(),
            "{} not below {}",
            new_recorder.total(),
            old_recorder.total()
        );
        assert_eq!(
            document.retained_bytes(),
            counts.text + 32 * counts.slots + 8 * counts.items + 24 * counts.members
        );
    }
    for index in 0..new_recorder.requests.len() {
        let mut denying = Recorder::denying(index);
        let outcome = read_exact_with_reservation(input, AMPLE, &mut denying);
        assert_eq!(
            outcome,
            Err(ReadError::ReservationDenied {
                requested: new_recorder.requests[index]
            })
        );
        assert_eq!(denying.requests, new_recorder.requests[..=index]);
    }
}

/// A JSON value to print, with names from a small set so that duplicate
/// names occur.
#[derive(Clone, Debug)]
enum Json {
    Null,
    Bool(bool),
    Number(&'static str),
    String(String),
    Array(Vec<Json>),
    Object(Vec<(String, Json)>),
}

fn text() -> impl Strategy<Value = String> {
    proptest::collection::vec(
        prop::sample::select(vec![
            'a', 'b', '~', '/', '"', '\\', '\n', '\u{1}', 'é', '😀',
        ]),
        0..4,
    )
    .prop_map(|characters| characters.into_iter().collect())
}

fn json() -> impl Strategy<Value = Json> {
    let leaf = prop_oneof![
        Just(Json::Null),
        any::<bool>().prop_map(Json::Bool),
        prop::sample::select(vec![
            "0",
            "-0",
            "12",
            "1.50",
            "1E2",
            "-3e-7",
            "12345678901234567890",
            "1e400",
            "-1e400",
        ])
        .prop_map(Json::Number),
        text().prop_map(Json::String),
    ];
    leaf.prop_recursive(5, 48, 6, |inner| {
        prop_oneof![
            proptest::collection::vec(inner.clone(), 0..6).prop_map(Json::Array),
            proptest::collection::vec(
                (
                    prop::sample::select(vec!["a", "b", "c", "~", "a/b", "é"])
                        .prop_map(String::from),
                    inner
                ),
                0..6
            )
            .prop_map(Json::Object),
        ]
    })
}

fn print_string(text: &str, out: &mut String) {
    out.push('"');
    for character in text.chars() {
        match character {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '/' => out.push_str("\\/"),
            '😀' => out.push_str("\\ud83d\\ude00"),
            character if u32::from(character) < 0x20 => {
                out.push_str(&format!("\\u{:04x}", u32::from(character)));
            }
            other => out.push(other),
        }
    }
    out.push('"');
}

fn print(value: &Json, space: &str, out: &mut String) {
    match value {
        Json::Null => out.push_str("null"),
        Json::Bool(value) => out.push_str(if *value { "true" } else { "false" }),
        Json::Number(text) => out.push_str(text),
        Json::String(text) => print_string(text, out),
        Json::Array(items) => {
            out.push('[');
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                    out.push_str(space);
                }
                print(item, space, out);
            }
            out.push(']');
        }
        Json::Object(members) => {
            out.push('{');
            for (index, (name, member)) in members.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                out.push_str(space);
                print_string(name, out);
                out.push(':');
                print(member, space, out);
            }
            out.push('}');
        }
    }
}

/// One edit at a fraction of the way through the input.
#[derive(Clone, Debug)]
enum Mutation {
    Insert(f64, u8),
    Delete(f64),
    Replace(f64, u8),
}

const JSON_BYTES: &[u8] = b"[]{}\",:0123456789-+.eEtrufalsn \\u/\xc3\xa9\xff";

fn mutation() -> impl Strategy<Value = Mutation> {
    let byte = prop::sample::select(JSON_BYTES.to_vec());
    prop_oneof![
        (0.0..1.0_f64, byte.clone()).prop_map(|(at, byte)| Mutation::Insert(at, byte)),
        (0.0..1.0_f64).prop_map(Mutation::Delete),
        (0.0..1.0_f64, byte).prop_map(|(at, byte)| Mutation::Replace(at, byte)),
    ]
}

fn mutate(mut bytes: Vec<u8>, mutations: &[Mutation]) -> Vec<u8> {
    for mutation in mutations {
        let at = |fraction: f64, length: usize| ((fraction * length as f64) as usize).min(length);
        match *mutation {
            Mutation::Insert(fraction, byte) => {
                let index = at(fraction, bytes.len());
                bytes.insert(index, byte);
            }
            Mutation::Delete(fraction) if !bytes.is_empty() => {
                let index = at(fraction, bytes.len() - 1);
                bytes.remove(index);
            }
            Mutation::Replace(fraction, byte) if !bytes.is_empty() => {
                let index = at(fraction, bytes.len() - 1);
                bytes[index] = byte;
            }
            _ => {}
        }
    }
    bytes
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(2048))]

    #[test]
    fn arbitrary_bytes_read_the_same(bytes in proptest::collection::vec(any::<u8>(), 0..48)) {
        check(&bytes);
    }

    #[test]
    fn json_alphabet_reads_the_same(
        bytes in proptest::collection::vec(prop::sample::select(JSON_BYTES.to_vec()), 0..48),
    ) {
        check(&bytes);
    }

    #[test]
    fn documents_read_the_same(value in json(), spaced in any::<bool>()) {
        let mut text = String::new();
        print(&value, if spaced { " \n" } else { "" }, &mut text);
        check(text.as_bytes());
    }

    #[test]
    fn mutated_documents_read_the_same(
        value in json(),
        mutations in proptest::collection::vec(mutation(), 1..4),
    ) {
        let mut text = String::new();
        print(&value, "", &mut text);
        check(&mutate(text.into_bytes(), &mutations));
    }
}

#[test]
fn fixed_inputs_read_the_same() {
    for input in [
        &b""[..],
        b"null",
        b" 1 ",
        b"\"\"",
        b"[]",
        b"{}",
        b"[[[[[[[[[[]]]]]]]]]]",
        b"{\"a\":{\"a\":{\"a\":{\"a\":{\"a\":[1,2,3]}}}}}",
        b"{\"a\":1,\"b\":2,\"a\":3}",
        b"[{\"a\":1,\"a\":2},1e400]",
        b"[{\"x\":[0,1e400]}]",
        b"{\"a\":1,\"b\":2} x",
        b"[{\"a\":1,\"b\":2},",
        b"[1,2,",
        b"\xef\xbb\xbf{}",
        b"\"\\ud800\"",
    ] {
        check(input);
    }
}

#[test]
fn the_input_limit_comes_first_and_asks_nothing() {
    for input in [
        &b"[1,2,3]"[..],
        b"[1,",
        b"\xff\xff\xff",
        b"{\"a\":1,\"a\":2}",
    ] {
        let mut recorder = Recorder::default();
        let outcome = read_exact_with_reservation(input, 2, &mut recorder);
        let Err(ReadError::Limit(limit)) = outcome else {
            panic!("{input:?}: {outcome:?}");
        };
        assert_eq!(limit.kind, LimitKind::InputBytes);
        assert_eq!(limit.required, input.len() as u64);
        assert!(recorder.requests.is_empty());
    }
}

/// The two outcome-order changes the docs name, under a tight budget.
#[test]
fn a_tight_budget_orders_outcomes_as_documented() {
    // Malformed, with no object of two members before the fault: pass 1
    // refuses it after asking only for its stack, where the growing reader
    // stops on the budget.
    let input = b"[\"a long enough string\",2,3,x]";
    let mut old = Recorder::denying(1);
    assert_eq!(
        read_with_reservation(input, AMPLE, &mut old),
        Err(ReadError::ReservationDenied {
            requested: old.requests[1]
        })
    );
    let mut new = Recorder::denying(1);
    assert_eq!(
        read_exact_with_reservation(input, AMPLE, &mut new),
        Err(ReadError::Malformed {
            offset: 28,
            kind: Malformed::UnexpectedCharacter
        })
    );
    assert_eq!(new.requests, [64]);
    // A scalar asks nothing at all before its fault.
    let mut nothing = Recorder::denying(0);
    assert!(matches!(
        read_exact_with_reservation(b"tru", AMPLE, &mut nothing),
        Err(ReadError::Malformed { .. })
    ));
    assert!(nothing.requests.is_empty());

    // A duplicate name early in a large document: the growing reader reaches
    // it with little charged; pass 2 reserves the arena for the whole input
    // first, which this budget refuses.
    let mut input = String::from("[{\"a\":1,\"a\":2}");
    for index in 0..1000 {
        input.push_str(&format!(",{index}"));
    }
    input.push(']');
    let mut old = Recorder::default();
    let duplicate = read_with_reservation(input.as_bytes(), AMPLE, &mut old);
    assert!(matches!(duplicate, Err(ReadError::DuplicateName { .. })));
    let bound = old.total();
    let mut budget = Budget { bound, charged: 0 };
    assert_eq!(
        read_with_reservation(input.as_bytes(), AMPLE, &mut budget),
        duplicate
    );
    let mut budget = Budget { bound, charged: 0 };
    assert!(matches!(
        read_exact_with_reservation(input.as_bytes(), AMPLE, &mut budget),
        Err(ReadError::ReservationDenied { .. })
    ));
    // With room for the arena it is the same refusal.
    let (new, _) = exact(input.as_bytes());
    assert_eq!(new, duplicate);
}

/// A root object of `members` members, each a small record.
fn records(members: usize) -> String {
    let mut text = String::from("{");
    for index in 0..members {
        if index > 0 {
            text.push(',');
        }
        text.push_str(&format!(
            "\"k{index:07}\":{{\"id\":{index},\"tags\":[\"x\",\"y\"],\"ok\":true}}"
        ));
    }
    text.push('}');
    text
}

/// The bytes asked by each reader at 1k, 10k and 100k members, against the
/// input and against each other. The ratios are printed with
/// `--nocapture`.
#[test]
fn exact_asks_less_than_growing_at_scale() {
    for members in [1_000, 10_000, 100_000] {
        let input = records(members);
        let (old, old_recorder) = growing(input.as_bytes());
        let (new, new_recorder) = exact(input.as_bytes());
        let document = new.expect("reads");
        assert_eq!(Ok(&document), old.as_ref());
        let counts = counts(&document);
        assert_eq!(new_recorder.requests, promised_requests(&counts));
        let (old_total, new_total) = (old_recorder.total(), new_recorder.total());
        let length = input.len() as f64;
        let retained_old = old.as_ref().map(Document::retained_bytes).unwrap_or(0);
        println!(
            "{members} members, {} input bytes: growing asks {:.2}x input, exact {:.2}x \
             (exact/growing {:.3}); retained growing {:.2}x, exact {:.2}x",
            input.len(),
            old_total as f64 / length,
            new_total as f64 / length,
            new_total as f64 / old_total as f64,
            retained_old as f64 / length,
            document.retained_bytes() as f64 / length,
        );
        assert!(new_total * 2 < old_total, "{new_total} vs {old_total}");
        assert!(document.retained_bytes() <= retained_old);
    }
}
