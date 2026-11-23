// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Agent-IX
//! `Document::extract_with_reservation`: each copy equals its source subtree
//! and encodes to the same canonical bytes, the returned ids name the copies
//! in order, the reservation is asked once per list for the exact size, a
//! denial at any call stops everything after it, and the source is untouched.

#![cfg(target_pointer_width = "64")]

use proptest::prelude::*;
use quire_canonical::{
    read, to_vec, Document, ExtractError, Limits, Node, NodeId, NodeRef, Reservation,
    ReservationDenied,
};
use serde_json::{Map, Number, Value};

const LIMITS: Limits = Limits::new(1 << 30);

/// Bytes per value, array element, object member and returned id on a 64-bit
/// target.
const SLOT: usize = 32;
const ITEM: usize = 8;
const MEMBER: usize = 24;
const ID: usize = 8;

/// Records every request in order and denies the one at `deny`.
#[derive(Default)]
struct Recorder {
    deny: Option<usize>,
    requests: Vec<usize>,
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

/// Every value of `document`, root first, from an explicit stack.
fn all_ids(document: &Document) -> Vec<NodeId> {
    let mut ids = Vec::new();
    let mut stack = vec![document.root()];
    while let Some(node) = stack.pop() {
        ids.push(node.id());
        match node.node() {
            Node::Array(items) => stack.extend(items),
            Node::Object(members) => stack.extend(members.map(|(_, value)| value)),
            _ => {}
        }
    }
    ids
}

/// The sizes the copy of `node` needs: decoded text bytes, values, array
/// elements and object members.
#[derive(Debug, Default, PartialEq)]
struct Sizes {
    text: usize,
    slots: usize,
    items: usize,
    members: usize,
}

fn sizes(node: NodeRef<'_>) -> Sizes {
    let mut sizes = Sizes::default();
    let mut stack = vec![node];
    while let Some(node) = stack.pop() {
        sizes.slots += 1;
        match node.node() {
            Node::Null | Node::Bool(_) => {}
            Node::Number(number) => sizes.text += number.text().len(),
            Node::String(text) => sizes.text += text.len(),
            Node::Array(items) => {
                sizes.items += items.len();
                stack.extend(items);
            }
            Node::Object(members) => {
                sizes.members += members.len();
                for (name, value) in members {
                    sizes.text += name.len();
                    stack.push(value);
                }
            }
        }
    }
    sizes
}

/// Structural equality of two subtrees: same kinds, same member names in the
/// same order, same strings, same number spellings and doubles.
fn same(left: NodeRef<'_>, right: NodeRef<'_>) -> bool {
    let mut stack = vec![(left, right)];
    while let Some((left, right)) = stack.pop() {
        match (left.node(), right.node()) {
            (Node::Null, Node::Null) => {}
            (Node::Bool(a), Node::Bool(b)) if a == b => {}
            (Node::Number(a), Node::Number(b))
                if a.text() == b.text() && a.value().to_bits() == b.value().to_bits() => {}
            (Node::String(a), Node::String(b)) if a == b => {}
            (Node::Array(a), Node::Array(b)) if a.len() == b.len() => stack.extend(a.zip(b)),
            (Node::Object(a), Node::Object(b)) if a.len() == b.len() => {
                for ((name_a, a), (name_b, b)) in a.zip(b) {
                    if name_a != name_b {
                        return false;
                    }
                    stack.push((a, b));
                }
            }
            _ => return false,
        }
    }
    true
}

/// The exact bytes the reservation is asked for, per list, zero lists left
/// out, in the order text, values, elements, members, ids.
fn expected_requests(source: &Document, roots: &[NodeId]) -> Vec<usize> {
    let mut sum = Sizes::default();
    for root in roots {
        let part = sizes(source.node(*root).expect("root"));
        sum.text += part.text;
        sum.slots += part.slots;
        sum.items += part.items;
        sum.members += part.members;
    }
    let k = roots.len();
    [
        sum.text,
        SLOT * (sum.slots + 1),
        ITEM * (sum.items + k),
        MEMBER * sum.members,
        ID * k,
    ]
    .into_iter()
    .filter(|bytes| *bytes > 0)
    .collect()
}

fn leaf() -> impl Strategy<Value = Value> {
    prop_oneof![
        Just(Value::Null),
        any::<bool>().prop_map(Value::Bool),
        any::<f64>()
            .prop_filter_map("finite", Number::from_f64)
            .prop_map(Value::Number),
        any::<i32>().prop_map(|value| Value::Number(value.into())),
        "[a-z\u{e9}\u{1f600}\"\\\\/~]{0,6}".prop_map(Value::String),
    ]
}

fn json() -> impl Strategy<Value = Value> {
    leaf().prop_recursive(5, 96, 6, |inner| {
        prop_oneof![
            prop::collection::vec(inner.clone(), 0..6).prop_map(Value::Array),
            prop::collection::vec(("[a-c~/]{0,3}", inner), 0..6)
                .prop_map(|members| Value::Object(members.into_iter().collect::<Map<_, _>>())),
        ]
    })
}

/// A document and a selection of its values (repeats and nesting allowed).
fn selection() -> impl Strategy<Value = (Document, Vec<NodeId>)> {
    (
        json(),
        prop::collection::vec(any::<prop::sample::Index>(), 0..6),
    )
        .prop_map(|(value, picks)| {
            let text = serde_json::to_string(&value).expect("serializes");
            let document = read(text.as_bytes(), u64::MAX).expect("reads");
            let ids = all_ids(&document);
            let roots = picks.iter().map(|pick| *pick.get(&ids)).collect();
            (document, roots)
        })
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]

    /// Properties 1 to 3: equal copies with equal canonical bytes, ids in
    /// order, one exact request per list.
    #[test]
    fn copies_are_equal_ids_ordered_and_sizes_exact((source, roots) in selection()) {
        let before = source.clone();
        let mut recorder = Recorder::default();
        let (extract, ids) = source
            .extract_with_reservation(&roots, &mut recorder)
            .expect("extracts");
        prop_assert_eq!(&source, &before);
        prop_assert_eq!(ids.len(), roots.len());

        let Node::Array(elements) = extract.root().node() else {
            return Err(TestCaseError::fail("root is not an array"));
        };
        let elements: Vec<NodeId> = elements.map(|element| element.id()).collect();
        prop_assert_eq!(&elements, &ids);

        for (root, id) in roots.iter().zip(&ids) {
            let original = source.node(*root).expect("root");
            let copy = extract.node(*id).expect("copy");
            prop_assert!(same(original, copy));
            prop_assert_eq!(
                to_vec(&original, LIMITS).expect("encodes"),
                to_vec(&copy, LIMITS).expect("encodes")
            );
        }
        // Repeated roots are copied apart: every returned id is distinct.
        let mut distinct = ids.clone();
        distinct.sort();
        distinct.dedup();
        prop_assert_eq!(distinct.len(), ids.len());

        let expected = expected_requests(&source, &roots);
        prop_assert_eq!(&recorder.requests, &expected);
        let total: usize = expected.iter().sum();
        prop_assert_eq!(extract.retained_bytes(), total - ID * roots.len());

        // An extract is a document like any other: extracting its copies
        // again gives the same bytes and the same size.
        let (again, again_ids) = extract
            .extract_with_reservation(&ids, &mut Recorder::default())
            .expect("extracts");
        prop_assert_eq!(
            to_vec(&again, LIMITS).expect("encodes"),
            to_vec(&extract, LIMITS).expect("encodes")
        );
        prop_assert_eq!(again.retained_bytes(), extract.retained_bytes());
        prop_assert_eq!(again_ids, ids);
    }

    /// Property 4: a denial at any call index is that denial, nothing is
    /// asked after it, and the source is unchanged.
    #[test]
    fn denial_at_every_call_stops_everything((source, roots) in selection()) {
        let before = source.clone();
        let expected = expected_requests(&source, &roots);
        for deny in 0..expected.len() {
            let mut recorder = Recorder { deny: Some(deny), ..Recorder::default() };
            let refused = source.extract_with_reservation(&roots, &mut recorder);
            prop_assert_eq!(
                refused.err(),
                Some(ExtractError::ReservationDenied { requested: expected[deny] })
            );
            prop_assert_eq!(&recorder.requests[..], &expected[..=deny]);
            prop_assert_eq!(&source, &before);
        }
    }
}

#[test]
fn an_unknown_root_is_refused_before_any_request() {
    let small = read(b"[1]", 64).expect("reads");
    let large = read(b"[1,2,3,4]", 64).expect("reads");
    let foreign = large.root().id();
    let mut recorder = Recorder::default();
    let refused = small.extract_with_reservation(&[small.root().id(), foreign], &mut recorder);
    assert_eq!(refused.err(), Some(ExtractError::UnknownNode(foreign)));
    assert!(recorder.requests.is_empty());
}

#[test]
fn no_roots_give_an_empty_array() {
    let source = read(br#"{"a":[1,2]}"#, 64).expect("reads");
    let mut recorder = Recorder::default();
    let (extract, ids) = source
        .extract_with_reservation(&[], &mut recorder)
        .expect("extracts");
    assert!(ids.is_empty());
    assert_eq!(to_vec(&extract, LIMITS).expect("encodes"), b"[]");
    assert_eq!(recorder.requests, [SLOT]);
    assert_eq!(extract.retained_bytes(), SLOT);
}

/// The extract's size is a function of the selected payload only: the
/// unselected remainder can grow by orders of magnitude without changing a
/// byte of it.
#[test]
fn retained_bytes_do_not_depend_on_the_unselected_remainder() {
    // Payload {"k":"vv","n":[1.5,null]}: text "k" "vv" "n" "1.5" = 7 bytes;
    // values: "vv", 1.5, null, [..], {..} = 5; elements 2; members 2.
    // Plus the result array: one value and one element.
    let payload_bytes = 7 + SLOT * (5 + 1) + ITEM * (2 + 1) + MEMBER * 2;
    assert_eq!(payload_bytes, 271);
    let mut previous = None;
    for remainder in [0_usize, 10, 1_000, 100_000] {
        let rest = vec![r#"{"x":"some unselected text","y":[1,2,3]}"#; remainder].join(",");
        let text = format!(r#"{{"payload":{{"k":"vv","n":[1.5,null]}},"rest":[{rest}]}}"#);
        let source = read(text.as_bytes(), u64::MAX).expect("reads");
        let payload = source.root().get("payload").expect("member").id();
        let mut recorder = Recorder::default();
        let (extract, ids) = source
            .extract_with_reservation(&[payload], &mut recorder)
            .expect("extracts");
        assert_eq!(extract.retained_bytes(), payload_bytes);
        assert_eq!(recorder.requests.iter().sum::<usize>(), payload_bytes + ID);
        assert_eq!(
            to_vec(&extract.node(ids[0]).expect("copy"), LIMITS).expect("encodes"),
            br#"{"k":"vv","n":[1.5,null]}"#
        );
        if let Some(previous) = previous {
            assert_eq!(extract, previous);
        }
        assert!(remainder < 1_000 || source.retained_bytes() > 100 * payload_bytes);
        previous = Some(extract);
    }
}

/// A subtree of any depth copies with no recursion: a chain of a hundred
/// thousand arrays, on a 64 KiB thread stack.
#[test]
fn depth_needs_no_stack() {
    const DEPTH: usize = 100_000;
    let text = format!("{}0{}", "[".repeat(DEPTH), "]".repeat(DEPTH));
    let source = read(text.as_bytes(), u64::MAX).expect("reads");
    let mut node = source.root();
    for _ in 0..DEPTH / 2 {
        let Node::Array(mut items) = node.node() else {
            panic!("an array")
        };
        node = items.next().expect("one element");
    }
    let middle = node.id();
    let extract = std::thread::Builder::new()
        .stack_size(64 * 1024)
        .spawn(move || {
            let (extract, ids) = source
                .extract_with_reservation(&[middle], &mut Recorder::default())
                .expect("extracts");
            assert_eq!(ids.len(), 1);
            extract
        })
        .expect("spawns")
        .join()
        .expect("joins");
    let expected = format!("[{}0{}]", "[".repeat(DEPTH / 2), "]".repeat(DEPTH / 2));
    assert_eq!(
        to_vec(&extract, LIMITS).expect("encodes"),
        expected.as_bytes()
    );
    // 50,001 values in the copy, plus the result array; 50,000 elements plus
    // the result array's one; text "0".
    assert_eq!(
        extract.retained_bytes(),
        1 + SLOT * (DEPTH / 2 + 2) + ITEM * (DEPTH / 2 + 1)
    );
}
