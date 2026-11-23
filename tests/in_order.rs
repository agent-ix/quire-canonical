// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Agent-IX
//! The in-order writer (`Writer::in_order_with_reservation`): its bytes are
//! the sorting writer's whenever members arrive in canonical order, it refuses
//! `Error::MemberOrder` exactly when they do not, its other refusals are the
//! sorting writer's, it streams objects, and a denied reservation stops it at
//! once.
//!
//! The heap it charges, against a counting allocator, is in
//! `tests/in_order_reservation.rs`.

use std::cmp::Ordering;
use std::collections::BTreeMap;

use proptest::prelude::*;
use quire_canonical::{
    encode_in_order_with_reservation, encode_with_reservation, read, Encode, Error, FixedShape,
    LimitKind, Limits, ProtocolViolation, Reservation, ReservationDenied, Sink, Writer,
};
use serde::Serialize;

/// A JSON value whose object members are kept in the order given.
#[derive(Clone, Debug)]
enum Json {
    Null,
    Bool(bool),
    Number(f64),
    Integer(i64),
    Text(String),
    Array(Vec<Json>),
    Object(Vec<(String, Json)>),
}

impl Encode for Json {
    fn encode_into<S: Sink + ?Sized>(&self, writer: &mut Writer<'_, S>) -> Result<(), Error> {
        // Test values are a few levels deep, so recursion is fine here.
        match self {
            Self::Null => writer.null(),
            Self::Bool(value) => writer.bool(*value),
            Self::Number(value) => writer.number(*value),
            Self::Integer(value) => writer.integer(i128::from(*value)),
            Self::Text(value) => writer.string(value),
            Self::Array(items) => {
                writer.begin_array()?;
                for item in items {
                    item.encode_into(writer)?;
                }
                writer.end_array()
            }
            Self::Object(members) => {
                writer.begin_object()?;
                for (name, value) in members {
                    writer.name(name)?;
                    value.encode_into(writer)?;
                }
                writer.end_object()
            }
        }
    }
}

/// RFC 8785 member order, computed independently of the crate.
fn utf16_order(left: &str, right: &str) -> Ordering {
    left.encode_utf16().cmp(right.encode_utf16())
}

/// `value` with every object's members sorted and repeated names dropped.
fn canonical(value: &Json) -> Json {
    match value {
        Json::Array(items) => Json::Array(items.iter().map(canonical).collect()),
        Json::Object(members) => {
            let mut sorted: Vec<(String, Json)> = members
                .iter()
                .map(|(name, value)| (name.clone(), canonical(value)))
                .collect();
            sorted.sort_by(|left, right| utf16_order(&left.0, &right.0));
            sorted.dedup_by(|right, left| left.0 == right.0);
            Json::Object(sorted)
        }
        other => other.clone(),
    }
}

/// The depth of the first object, in event order, whose next name does not
/// sort strictly after the previous one: the expected `MemberOrder` depth.
fn first_violation(value: &Json, depth: usize) -> Option<usize> {
    match value {
        Json::Array(items) => items
            .iter()
            .find_map(|item| first_violation(item, depth + 1)),
        Json::Object(members) => {
            let mut previous: Option<&str> = None;
            for (name, value) in members {
                if previous.is_some_and(|previous| utf16_order(previous, name).is_ge()) {
                    return Some(depth);
                }
                previous = Some(name);
                if let Some(found) = first_violation(value, depth + 1) {
                    return Some(found);
                }
            }
            None
        }
        _ => None,
    }
}

/// A reservation that admits everything, or denies its `deny_at`-th call
/// (from 0) and counts every call it gets.
struct Hook {
    deny_at: Option<usize>,
    calls: usize,
    charged: usize,
}

impl Hook {
    fn admitting() -> Self {
        Self {
            deny_at: None,
            calls: 0,
            charged: 0,
        }
    }

    fn denying_call(index: usize) -> Self {
        Self {
            deny_at: Some(index),
            calls: 0,
            charged: 0,
        }
    }
}

impl Reservation for Hook {
    fn reserve(&mut self, bytes: usize) -> Result<(), ReservationDenied> {
        let call = self.calls;
        self.calls += 1;
        if self.deny_at == Some(call) {
            return Err(ReservationDenied);
        }
        self.charged += bytes;
        Ok(())
    }
}

const LIMITS: Limits = Limits::new(1 << 24);

/// The sorting writer's outcome and bytes.
fn sorted_bytes<T: Encode + ?Sized>(value: &T, limits: Limits) -> (Result<u64, Error>, Vec<u8>) {
    let mut bytes = Vec::new();
    let outcome = encode_with_reservation(&mut bytes, value, limits, &mut Hook::admitting());
    (outcome, bytes)
}

/// The in-order writer's outcome and whatever reached the sink.
fn in_order_bytes<T: Encode + ?Sized>(value: &T, limits: Limits) -> (Result<u64, Error>, Vec<u8>) {
    let mut bytes = Vec::new();
    let outcome =
        encode_in_order_with_reservation(&mut bytes, value, limits, &mut Hook::admitting());
    (outcome, bytes)
}

/// Names that exercise UTF-16 order: escapes, controls, and the high-BMP
/// against supplementary-plane inversion of UTF-8 byte order.
const PIECES: [&str; 12] = [
    "a",
    "b",
    "ab",
    "\"",
    "\\",
    "\n",
    "\u{7f}",
    "é",
    "\u{e000}",
    "\u{ffff}",
    "\u{10000}",
    "\u{1f600}",
];

fn name() -> impl Strategy<Value = String> {
    prop::collection::vec(prop::sample::select(&PIECES[..]), 0..3).prop_map(|parts| parts.concat())
}

fn leaf() -> impl Strategy<Value = Json> {
    prop_oneof![
        Just(Json::Null),
        any::<bool>().prop_map(Json::Bool),
        any::<f64>()
            .prop_filter("finite", |value| value.is_finite())
            .prop_map(Json::Number),
        (-(1_i64 << 53)..=(1_i64 << 53)).prop_map(Json::Integer),
        name().prop_map(Json::Text),
    ]
}

/// An arbitrary value; object members in any order, names may repeat.
fn json() -> impl Strategy<Value = Json> {
    leaf().prop_recursive(4, 64, 6, |inner| {
        prop_oneof![
            prop::collection::vec(inner.clone(), 0..6).prop_map(Json::Array),
            prop::collection::vec((name(), inner), 0..6).prop_map(Json::Object),
        ]
    })
}

/// Distinct names in canonical order.
fn sorted_names() -> impl Strategy<Value = Vec<String>> {
    prop::collection::btree_set(name(), 2..8).prop_map(|names| {
        let mut names: Vec<String> = names.into_iter().collect();
        names.sort_by(|left, right| utf16_order(left, right));
        names
    })
}

/// `{"outer":[<object of names>]}`: the permuted object sits at depth 2.
fn nested(names: &[String]) -> Json {
    let inner = Json::Object(
        names
            .iter()
            .map(|name| (name.clone(), Json::Integer(1)))
            .collect(),
    );
    Json::Object(vec![("outer".to_owned(), Json::Array(vec![inner]))])
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]

    /// (a) With members in canonical order, the in-order bytes are the
    /// sorting writer's bytes.
    #[test]
    fn in_order_bytes_equal_sorted_bytes(value in json()) {
        let value = canonical(&value);
        let (sorted, expected) = sorted_bytes(&value, LIMITS);
        let (outcome, bytes) = in_order_bytes(&value, LIMITS);
        prop_assert_eq!(outcome.expect("in order"), sorted.expect("sorts"));
        prop_assert_eq!(bytes, expected);
    }

    /// (b) For members in any order, the in-order writer succeeds exactly
    /// when every object's names strictly increase, refusing `MemberOrder`
    /// at the first object where they do not; when it succeeds its bytes are
    /// the sorting writer's.
    #[test]
    fn member_order_refused_exactly_when_not_canonical(value in json()) {
        let (outcome, bytes) = in_order_bytes(&value, LIMITS);
        match first_violation(&value, 0) {
            None => {
                let (sorted, expected) = sorted_bytes(&value, LIMITS);
                prop_assert_eq!(outcome.expect("in order"), sorted.expect("sorts"));
                prop_assert_eq!(bytes, expected);
            }
            Some(depth) => {
                prop_assert!(
                    matches!(outcome, Err(Error::MemberOrder { depth: found }) if found == depth),
                    "expected MemberOrder at depth {}, got {:?}", depth, outcome
                );
            }
        }
    }

    /// (b) Every permutation of an object's names other than canonical order
    /// is refused; canonical order is accepted.
    #[test]
    fn non_canonical_permutation_is_refused(
        (names, permuted) in sorted_names()
            .prop_flat_map(|names| (Just(names.clone()), Just(names).prop_shuffle()))
    ) {
        let (outcome, bytes) = in_order_bytes(&nested(&permuted), LIMITS);
        if permuted == names {
            let (_, expected) = sorted_bytes(&nested(&names), LIMITS);
            prop_assert_eq!(bytes, expected);
            prop_assert!(outcome.is_ok());
        } else {
            prop_assert!(
                matches!(outcome, Err(Error::MemberOrder { depth: 2 })),
                "{:?}", outcome
            );
        }
    }

    /// (b) A repeated name, anywhere after its first use, is refused.
    #[test]
    fn duplicate_name_is_refused(
        (names, from, to) in sorted_names()
            .prop_flat_map(|names| {
                let length = names.len();
                (Just(names), 0..length, 0..=length)
            })
    ) {
        let mut with_duplicate = names.clone();
        let repeated = names[from].clone();
        // Insert after the first use, so the first use stays in order.
        with_duplicate.insert(to.max(from + 1), repeated);
        let (outcome, _) = in_order_bytes(&nested(&with_duplicate), LIMITS);
        prop_assert!(
            matches!(outcome, Err(Error::MemberOrder { depth: 2 })),
            "{:?}", outcome
        );
        // The sorting writer refuses the same value as a duplicate.
        let (sorted, _) = sorted_bytes(&nested(&with_duplicate), LIMITS);
        prop_assert!(
            matches!(sorted, Err(Error::DuplicateMemberName { .. })),
            "{:?}", sorted
        );
    }

    /// (c) A reservation that denies at any one of the writer's growths
    /// refuses with `ReservationDenied` at that growth, and the writer asks
    /// for nothing after it. (`tests/in_order_reservation.rs` checks against
    /// a counting allocator that nothing is allocated for it or kept.)
    #[test]
    fn a_denied_growth_stops_the_writer(value in json()) {
        let value = canonical(&value);
        let mut total = Hook::admitting();
        encode_in_order_with_reservation(&mut Vec::new(), &value, LIMITS, &mut total)
            .expect("admitted");
        for denied in 0..total.calls {
            let mut hook = Hook::denying_call(denied);
            let outcome =
                encode_in_order_with_reservation(&mut Vec::new(), &value, LIMITS, &mut hook);
            prop_assert!(
                matches!(outcome, Err(Error::ReservationDenied { requested }) if requested > 0),
                "call {}: {:?}", denied, outcome
            );
            prop_assert_eq!(hook.calls, denied + 1, "asked again after the denial");
        }
    }

    /// Under every byte ceiling the in-order writer succeeds exactly when the
    /// sorting writer does, refuses with the same limit kind and bound, and
    /// never hands the sink more than the ceiling.
    #[test]
    fn byte_ceiling_decides_the_same(value in json(), slack in 0_u64..4) {
        let value = canonical(&value);
        let (full, _) = sorted_bytes(&value, LIMITS);
        let length = full.expect("sorts");
        for ceiling in [0, length / 2, length.saturating_sub(1), length, length + slack] {
            let limits = Limits::new(ceiling);
            let (sorted, _) = sorted_bytes(&value, limits);
            let (outcome, bytes) = in_order_bytes(&value, limits);
            prop_assert!(bytes.len() as u64 <= ceiling);
            match (sorted, outcome) {
                (Ok(left), Ok(right)) => prop_assert_eq!(left, right),
                (Err(Error::Limit(left)), Err(Error::Limit(right))) => {
                    prop_assert_eq!(left.kind, LimitKind::CanonicalBytes);
                    prop_assert_eq!(right.kind, LimitKind::CanonicalBytes);
                    prop_assert_eq!(left.bound, right.bound);
                }
                (left, right) => prop_assert!(false, "{:?} vs {:?}", left, right),
            }
        }
    }
}

/// Run `push` through a sorting and an in-order writer and return both
/// outcomes, `finish` included, as `Debug` text (`Error` is not `PartialEq`).
fn both(push: impl Fn(&mut Writer<'_, Vec<u8>>) -> Result<(), Error>) -> (String, String) {
    let run = |in_order: bool| {
        let mut bytes = Vec::new();
        let mut hook = Hook::admitting();
        let mut writer = if in_order {
            Writer::in_order_with_reservation(&mut bytes, LIMITS, &mut hook)
        } else {
            Writer::with_reservation(&mut bytes, LIMITS, &mut hook)
        };
        let outcome = push(&mut writer).and_then(|()| writer.finish());
        format!("{outcome:?}")
    };
    (run(false), run(true))
}

#[derive(Serialize, FixedShape)]
struct Sorted {
    a: u8,
    b: &'static str,
}

#[derive(Serialize, FixedShape)]
struct Unsorted {
    b: u8,
    a: u8,
}

#[derive(Serialize, FixedShape)]
#[allow(dead_code)]
enum Tagged {
    Fields { a: u8, b: bool },
}

/// The scalar, serde and protocol refusals are the sorting writer's, inside
/// an in-order object as well as at the top level.
#[test]
fn refusals_match_the_sorting_writer() {
    type Push = fn(&mut Writer<'_, Vec<u8>>) -> Result<(), Error>;
    let in_object = |writer: &mut Writer<'_, Vec<u8>>| {
        writer.begin_object()?;
        writer.name("a")
    };
    let cases: [(&str, Push); 16] = [
        ("NaN", |writer| writer.number(f64::NAN)),
        ("infinity", |writer| {
            writer.begin_object()?;
            writer.name("a")?;
            writer.number(f64::INFINITY)
        }),
        ("integer past 2^53", |writer| {
            writer.begin_object()?;
            writer.name("a")?;
            writer.integer((1 << 53) + 1)
        }),
        ("negative integer past 2^53", |writer| {
            writer.integer(-(1 << 53) - 1)
        }),
        ("u128 past 2^53", |writer| {
            writer.begin_object()?;
            writer.name("a")?;
            writer.serialize(&u128::MAX)
        }),
        ("non-string member name", |writer| {
            writer.serialize(&BTreeMap::from([(true, 1_u8)]))
        }),
        ("value without name", |writer| {
            writer.begin_object()?;
            writer.null()
        }),
        ("name without value", |writer| {
            writer.begin_object()?;
            writer.name("a")?;
            writer.name("b")
        }),
        ("object ended after name", |writer| {
            writer.begin_object()?;
            writer.name("a")?;
            writer.end_object()
        }),
        ("name outside object", |writer| {
            writer.begin_array()?;
            writer.name("a")
        }),
        ("end_array in object", |writer| {
            writer.begin_object()?;
            writer.end_array()
        }),
        ("end_object in array", |writer| {
            writer.begin_array()?;
            writer.end_object()
        }),
        ("second value", |writer| {
            writer.begin_object()?;
            writer.end_object()?;
            writer.null()
        }),
        ("incomplete", |writer| {
            writer.begin_object()?;
            writer.name("a")?;
            writer.begin_object()
        }),
        ("nothing", |_| Ok(())),
        ("after refusal", |writer| {
            let _ = writer.number(f64::NAN);
            writer.null()
        }),
    ];
    for (case, push) in cases {
        let (sorted, in_order) = both(push);
        assert!(sorted.starts_with("Err("), "{case}: {sorted}");
        assert_eq!(in_order, sorted, "{case}");
    }
    let (sorted, in_order) = both(|writer| {
        in_object(writer)?;
        writer.number(f64::NEG_INFINITY)
    });
    assert_eq!(in_order, sorted);

    // A derived struct streams when its fields are declared in canonical
    // order, and is refused when they are not.
    assert_eq!(
        in_order_bytes(&Sorted { a: 1, b: "x" }, LIMITS).1,
        sorted_bytes(&Sorted { a: 1, b: "x" }, LIMITS).1
    );
    assert!(matches!(
        in_order_bytes(&Unsorted { b: 1, a: 2 }, LIMITS).0,
        Err(Error::MemberOrder { depth: 0 })
    ));
    // serde's one-member enum wrapper holds an in-order object.
    let (outcome, bytes) = in_order_bytes(&Tagged::Fields { a: 1, b: true }, LIMITS);
    assert_eq!(outcome.expect("streams"), bytes.len() as u64);
    assert_eq!(bytes, br#"{"Fields":{"a":1,"b":true}}"#);
}

/// An object's bytes reach the sink as they are produced, before it closes.
#[test]
fn objects_stream_to_the_sink() {
    let mut bytes = Vec::new();
    let mut hook = Hook::admitting();
    {
        let mut writer = Writer::in_order_with_reservation(&mut bytes, LIMITS, &mut hook);
        writer.begin_object().expect("opens");
        writer.name("a").expect("names");
        writer.begin_object().expect("opens");
        writer.name("b").expect("names");
        writer.integer(1).expect("writes");
        writer.name("c").expect("names");
        assert_eq!(writer.produced(), 16);
        writer.null().expect("writes");
        writer.end_object().expect("closes");
        writer.end_object().expect("closes");
        assert_eq!(writer.finish().expect("complete"), 22);
    }
    assert_eq!(bytes, br#"{"a":{"b":1,"c":null}}"#);

    let mut bytes = Vec::new();
    {
        let mut writer = Writer::in_order_with_reservation(&mut bytes, LIMITS, &mut hook);
        writer.begin_object().expect("opens");
        writer.name("b").expect("names");
        writer.integer(1).expect("writes");
        // The refusal leaves the prefix produced before it.
        assert!(matches!(
            writer.name("a"),
            Err(Error::MemberOrder { depth: 0 })
        ));
        assert!(matches!(
            writer.null(),
            Err(Error::Protocol(ProtocolViolation::AfterRefusal))
        ));
        assert!(matches!(
            writer.finish(),
            Err(Error::Protocol(ProtocolViolation::AfterRefusal))
        ));
    }
    assert_eq!(bytes, br#"{"b":1"#);
}

/// A name is compared with the previous name of its own object only: a
/// nested object starts afresh, and its names leave when it closes.
#[test]
fn each_object_orders_its_own_names() {
    let value = Json::Object(vec![
        (
            "m".to_owned(),
            Json::Object(vec![
                ("z".to_owned(), Json::Null),
                (
                    "zz".to_owned(),
                    Json::Object(vec![("a".to_owned(), Json::Null)]),
                ),
            ]),
        ),
        (
            "n".to_owned(),
            Json::Object(vec![("a".to_owned(), Json::Null)]),
        ),
    ]);
    let (outcome, bytes) = in_order_bytes(&value, LIMITS);
    assert_eq!(outcome.expect("in order"), bytes.len() as u64);
    assert_eq!(bytes, sorted_bytes(&value, LIMITS).1);

    // UTF-16 order, not UTF-8 byte order: U+10000 sorts before U+FFFF.
    let value = Json::Object(vec![
        ("\u{10000}".to_owned(), Json::Null),
        ("\u{ffff}".to_owned(), Json::Null),
    ]);
    assert!(in_order_bytes(&value, LIMITS).0.is_ok());
    let value = Json::Object(vec![
        ("\u{ffff}".to_owned(), Json::Null),
        ("\u{10000}".to_owned(), Json::Null),
    ]);
    assert!(matches!(
        in_order_bytes(&value, LIMITS).0,
        Err(Error::MemberOrder { depth: 0 })
    ));
}

/// Re-encoding a document read from canonical text reproduces the text; a
/// document whose members are out of order is refused.
#[test]
fn a_canonical_document_round_trips() {
    let canonical = r#"{"":[],"a":[1,{"c":null,"d":true}],"b":"x","é":{"\u0000":-0.5}}"#;
    let document = read(canonical.as_bytes(), 1 << 10).expect("reads");
    let (outcome, bytes) = in_order_bytes(&document, LIMITS);
    assert_eq!(outcome.expect("canonical"), canonical.len() as u64);
    assert_eq!(bytes, canonical.as_bytes());

    let document = read(br#"{"a":{"d":1,"c":2}}"#, 1 << 10).expect("reads");
    assert!(matches!(
        in_order_bytes(&document, LIMITS).0,
        Err(Error::MemberOrder { depth: 1 })
    ));
}

#[test]
fn member_order_displays_its_depth() {
    assert_eq!(
        Error::MemberOrder { depth: 3 }.to_string(),
        "member name out of canonical order in an object at depth 3"
    );
}
