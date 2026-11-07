// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Agent-IX
//! The reader's working-byte provision: the encoded limit comes first, the
//! reservation is asked before each growth and for exactly the capacity
//! allocated, a denial leaves nothing behind, and a read in budget is the
//! read without one.
//!
//! A counting global allocator measures the real heap, so the binary holds one
//! test and no other test allocates concurrently.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};

use quire_canonical::{
    read, read_bounded, read_with_reservation, to_vec, LimitKind, Limits, Malformed, ReadError,
    Reservation, ReservationDenied,
};

struct Counting;

static CURRENT: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);

// SAFETY: every method forwards to `System` with the caller's arguments
// unchanged, so `System`'s guarantees hold; the counters are bookkeeping only.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // SAFETY: forwarded unchanged; the caller upholds `alloc`'s contract.
        let pointer = unsafe { System.alloc(layout) };
        if !pointer.is_null() {
            let now = CURRENT.fetch_add(layout.size(), Ordering::SeqCst) + layout.size();
            PEAK.fetch_max(now, Ordering::SeqCst);
        }
        pointer
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        // SAFETY: forwarded unchanged; the caller upholds `dealloc`'s contract.
        unsafe { System.dealloc(pointer, layout) };
        CURRENT.fetch_sub(layout.size(), Ordering::SeqCst);
    }

    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        // SAFETY: forwarded unchanged; the caller upholds `realloc`'s contract.
        let moved = unsafe { System.realloc(pointer, layout, new_size) };
        if !moved.is_null() {
            CURRENT.fetch_sub(layout.size(), Ordering::SeqCst);
            let now = CURRENT.fetch_add(new_size, Ordering::SeqCst) + new_size;
            PEAK.fetch_max(now, Ordering::SeqCst);
        }
        moved
    }
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

/// Records every request, in order, and denies the one at `deny`. Each
/// request also records how much heap was live when it was asked, above the
/// baseline taken when the recorder was made.
struct Recorder {
    baseline: usize,
    deny: Option<usize>,
    requests: Vec<usize>,
    live_when_asked: Vec<usize>,
    /// The most heap live above the baseline during the last `recorded` read.
    peak: usize,
}

impl Recorder {
    fn new(deny: Option<usize>) -> Self {
        let requests = Vec::with_capacity(4096);
        let live_when_asked = Vec::with_capacity(4096);
        Self {
            baseline: CURRENT.load(Ordering::SeqCst),
            deny,
            requests,
            live_when_asked,
            peak: 0,
        }
    }

    fn total(&self) -> usize {
        self.requests.iter().sum()
    }
}

impl Reservation for Recorder {
    fn reserve(&mut self, bytes: usize) -> Result<(), ReservationDenied> {
        let index = self.requests.len();
        self.live_when_asked
            .push(CURRENT.load(Ordering::SeqCst) - self.baseline);
        self.requests.push(bytes);
        if self.deny == Some(index) {
            return Err(ReservationDenied);
        }
        Ok(())
    }
}

const AMPLE: u64 = u64::MAX;

/// Read `input` asking a recorder that denies request `deny`; the recorder
/// is returned with the heap live once the outcome is dropped.
fn recorded(input: &[u8], deny: Option<usize>) -> (Result<(), ReadError>, Recorder, usize) {
    let mut recorder = Recorder::new(deny);
    PEAK.store(recorder.baseline, Ordering::SeqCst);
    let outcome = read_with_reservation(input, AMPLE, &mut recorder).map(drop);
    let after = CURRENT.load(Ordering::SeqCst);
    let live = after - recorder.baseline;
    recorder.peak = PEAK.load(Ordering::SeqCst) - recorder.baseline;
    (outcome, recorder, live)
}

/// Inputs that each force one kind of growth first: the decoded text, a
/// node, an open container, a child id and an object member.
const SHAPES: &[(&str, &str)] = &[
    ("decoded text", "\"a long enough string\""),
    ("node", "null"),
    ("open container", "[]"),
    ("array item", "[1,2,3,4,5]"),
    ("object member", "{\"a\":1,\"b\":2,\"c\":3,\"d\":4,\"e\":5}"),
    ("nested", "{\"k\":[1,{\"x\":\"y\"},[[]]],\"j\":null}"),
    // Escapes decode into text after the plain run before them was charged.
    (
        "escaped text",
        "\"tab\\there \\u00e9 \\ud83d\\ude00 and a long enough tail\"",
    ),
    (
        "escaped names",
        "{\"a\\nb\":\"\\u0041\\\\\",\"c\\/d\":[\"\\\"\"]}",
    ),
];

#[test]
fn reader_provision_is_charged_before_each_growth_and_a_denial_leaves_nothing() {
    encoded_limit_comes_before_parsing();
    for (name, text) in SHAPES {
        every_growth_is_asked_before_it_happens_and_denial_leaves_nothing(name, text);
    }
    first_growths_are_the_capacity_allocated();
    in_budget_reads_match_the_unmetered_reader();
    refusals_are_unchanged_in_budget();
    error_paths_charge_their_pointer_and_lexeme();
    bounded_reads_name_the_limit_and_required_bytes();
    deep_inputs_read_on_a_small_stack();
    the_document_keeps_what_the_reader_charged_for();
}

fn encoded_limit_comes_before_parsing() {
    for input in [
        &b"[1,2,3]"[..],
        b"[1,",
        b"\xff\xff\xff",
        b"{\"a\":1,\"a\":2}",
    ] {
        let mut recorder = Recorder::new(None);
        let outcome = read_with_reservation(input, 2, &mut recorder);
        let Err(ReadError::Limit(limit)) = outcome else {
            panic!("{input:?}: {outcome:?}");
        };
        assert_eq!(limit.kind, LimitKind::InputBytes);
        assert_eq!(limit.bound, 2);
        assert_eq!(limit.required, input.len() as u64);
        assert!(recorder.requests.is_empty(), "{input:?} was parsed");
        assert!(matches!(
            read_bounded(input, 2, AMPLE),
            Err(ReadError::Limit(limit)) if limit.kind == LimitKind::InputBytes
        ));
    }
}

fn every_growth_is_asked_before_it_happens_and_denial_leaves_nothing(name: &str, text: &str) {
    let (outcome, recorder, _) = recorded(text.as_bytes(), None);
    outcome.expect("admitted");
    let total = recorder.total();
    assert!(total > 0, "{name}: nothing was asked");
    // Before each request the reader holds no more heap than was already
    // charged: the growth it asks for has not happened.
    let mut charged = 0;
    for (index, bytes) in recorder.requests.iter().enumerate() {
        assert!(
            recorder.live_when_asked[index] <= charged,
            "{name}: request {index} came after allocating ({} live, {charged} charged)",
            recorder.live_when_asked[index]
        );
        charged += bytes;
    }
    // The charge is an upper bound on the peak, checked over the whole read:
    // an allocation after the last request, or one a freed block's slack
    // would hide from the per-request check, shows here.
    assert!(
        recorder.peak <= total,
        "{name}: peak heap {} above the {total} charged",
        recorder.peak
    );
    // Denying any one request refuses with that request's bytes, keeps no
    // heap and returns no document.
    for index in 0..recorder.requests.len() {
        let (outcome, denying, live) = recorded(text.as_bytes(), Some(index));
        assert!(
            matches!(
                outcome,
                Err(ReadError::ReservationDenied { requested }) if requested == recorder.requests[index]
            ),
            "{name}: request {index}: {outcome:?}"
        );
        assert_eq!(live, 0, "{name}: request {index} kept heap");
        assert_eq!(
            denying.requests.len(),
            index + 1,
            "{name}: asked after a denial"
        );
    }
}

/// The exact growth sequence of small inputs: capacity doubles from four, and
/// each request is element size times the full new capacity.
fn first_growths_are_the_capacity_allocated() {
    // Decoded text of 20 bytes is asked as 20, then the node as four slots.
    let (_, text, _) = recorded(b"\"a long enough string\"", None);
    assert_eq!(text.requests, [20, 4 * 32]);
    // An open container is one stack entry grown to four.
    let (_, array, _) = recorded(b"[]", None);
    assert_eq!(array.requests[0], 4 * 16);
    // `[1]`: stack, number text, node, scratch child id, final child list.
    let (_, item, _) = recorded(b"[1]", None);
    assert_eq!(item.requests, [4 * 16, 4, 4 * 32, 4 * 8, 4 * 8]);
    // A growth past four doubles and is asked in full: the fifth item takes
    // the scratch child list to eight ids, 64 bytes, not the added 32; the
    // closed array's list is sized to its five children, 40 bytes.
    let (_, items, _) = recorded(b"[1,2,3,4,5]", None);
    assert_eq!(items.requests, [64, 4, 128, 32, 8, 256, 64, 40]);
}

fn in_budget_reads_match_the_unmetered_reader() {
    for (name, text) in SHAPES {
        let plain = read(text.as_bytes(), AMPLE).expect("plain");
        let mut recorder = Recorder::new(None);
        let metered =
            read_with_reservation(text.as_bytes(), AMPLE, &mut recorder).expect("metered");
        assert_eq!(plain, metered, "{name}");
        let bounded =
            read_bounded(text.as_bytes(), AMPLE, recorder.total() as u64).expect("exact provision");
        assert_eq!(plain, bounded, "{name}");
        let limits = Limits::new(1 << 20);
        assert_eq!(
            to_vec(&plain, limits).expect("plain bytes"),
            to_vec(&metered, limits).expect("metered bytes"),
            "{name}"
        );
    }
}

fn refusals_are_unchanged_in_budget() {
    let cases: &[(&[u8], Malformed, usize)] = &[
        (b"[\"\xff\"]", Malformed::InvalidUtf8, 2),
        (b"[\"\\q\"]", Malformed::InvalidEscape, 2),
        (b"[1.]", Malformed::InvalidNumber, 1),
        (b"[1] 2", Malformed::TrailingContent, 4),
    ];
    let duplicate = ReadError::DuplicateName {
        offset: 7,
        pointer: "/a".into(),
    };
    assert_eq!(
        read(br#"{"a":1,"a":2}"#, AMPLE).expect_err("plain"),
        duplicate
    );
    let mut recorder = Recorder::new(None);
    assert_eq!(
        read_with_reservation(br#"{"a":1,"a":2}"#, AMPLE, &mut recorder).expect_err("metered"),
        duplicate
    );
    for (input, kind, offset) in cases {
        let expected = ReadError::Malformed {
            offset: *offset,
            kind: *kind,
        };
        assert_eq!(read(input, AMPLE).expect_err("plain"), expected);
        let mut recorder = Recorder::new(None);
        assert_eq!(
            read_with_reservation(input, AMPLE, &mut recorder).expect_err("metered"),
            expected,
            "{input:?}"
        );
    }
    let plain = read(b"[[1e400]]", AMPLE).expect_err("plain");
    let mut recorder = Recorder::new(None);
    let metered = read_with_reservation(b"[[1e400]]", AMPLE, &mut recorder).expect_err("metered");
    assert_eq!(plain, metered);
    assert!(matches!(plain, ReadError::NumberOutOfRange { .. }));
}

/// The pointer and lexeme a refusal carries, and the steps list that builds
/// the pointer, are asked for too: the request sequence is exact, every
/// denial returns that request's bytes and leaves no heap, and the charge
/// bounds the peak.
fn error_paths_charge_their_pointer_and_lexeme() {
    // `[[1e400]]`: stack (4 x 16), steps (4 x 24, an index step each for the
    // two arrays; the number's text is never appended), pointer "/0/0" (4), lexeme "1e400" (5).
    let (outcome, recorder, _) = recorded(b"[[1e400]]", None);
    assert!(matches!(outcome, Err(ReadError::NumberOutOfRange { .. })));
    assert_eq!(recorder.requests, [64, 96, 4, 5]);
    // `[{"k":1,"k":2}]`: the sort buffer, then steps for the repeated name
    // and the two open containers, and the pointer "/0/k".
    let (outcome, duplicate, _) = recorded(br#"[{"k":1,"k":2}]"#, None);
    assert!(matches!(outcome, Err(ReadError::DuplicateName { .. })));
    assert_eq!(duplicate.requests.last().copied(), Some(4));
    for (input, log) in [
        (&b"[[1e400]]"[..], &recorder),
        (br#"[{"k":1,"k":2}]"#, &duplicate),
    ] {
        assert!(log.peak <= log.total(), "{input:?}: peak {}", log.peak);
        for index in 0..log.requests.len() {
            let (outcome, denying, live) = recorded(input, Some(index));
            assert!(
                matches!(
                    outcome,
                    Err(ReadError::ReservationDenied { requested }) if requested == log.requests[index]
                ),
                "{input:?}: request {index}: {outcome:?}"
            );
            assert_eq!(live, 0, "{input:?}: request {index} kept heap");
            assert_eq!(denying.requests.len(), index + 1);
        }
    }
}

fn bounded_reads_name_the_limit_and_required_bytes() {
    for (name, text) in SHAPES {
        let (_, recorder, _) = recorded(text.as_bytes(), None);
        let total = recorder.total() as u64;
        read_bounded(text.as_bytes(), AMPLE, total).unwrap_or_else(|e| panic!("{name}: {e}"));
        let mut running = 0_u64;
        for (index, bytes) in recorder.requests.iter().enumerate() {
            // A bound just under the running total refuses at this request.
            let bound = running + *bytes as u64 - 1;
            let outcome = read_bounded(text.as_bytes(), AMPLE, bound);
            let Err(ReadError::Limit(limit)) = outcome else {
                panic!("{name}: request {index}: {outcome:?}");
            };
            assert_eq!(limit.kind, LimitKind::WorkingBytes);
            assert_eq!(limit.bound, bound);
            assert_eq!(limit.required, running + *bytes as u64);
            running += *bytes as u64;
        }
    }
}

fn nested(depth: usize, open: u8, close: u8, member: &[u8]) -> Vec<u8> {
    let mut text = Vec::new();
    for _ in 0..depth {
        text.push(open);
        text.extend_from_slice(member);
    }
    text.extend_from_slice(b"0");
    for _ in 0..depth {
        text.push(close);
    }
    text
}

fn deep_inputs_read_on_a_small_stack() {
    let depth = 100_000;
    let arrays = nested(depth, b'[', b']', b"");
    let objects = nested(depth, b'{', b'}', b"\"k\":");
    for input in [arrays, objects] {
        let (sufficient, denied) = std::thread::Builder::new()
            .stack_size(256 * 1024)
            .spawn(move || {
                let mut recorder = Recorder::new(None);
                let document = read_with_reservation(&input, AMPLE, &mut recorder).expect("deep");
                let total = recorder.total() as u64;
                let exact = read_bounded(&input, AMPLE, total).expect("exact provision");
                assert_eq!(document, exact);
                let denied = read_bounded(&input, AMPLE, total - 1);
                (total, denied.map(drop))
            })
            .expect("spawn")
            .join()
            .expect("no stack overflow");
        assert!(sufficient > 0);
        assert!(
            matches!(denied, Err(ReadError::Limit(limit)) if limit.kind == LimitKind::WorkingBytes),
            "{denied:?}"
        );
    }
}

/// The document holds exactly the capacity it reports, which is part of what
/// the reader charged; the rest was scratch that the read released.
fn the_document_keeps_what_the_reader_charged_for() {
    let text = SHAPES[5].1.as_bytes();
    let mut recorder = Recorder::new(None);
    let document = read_with_reservation(text, AMPLE, &mut recorder).expect("read");
    let held = CURRENT.load(Ordering::SeqCst) - recorder.baseline;
    assert_eq!(held, document.retained_bytes());
    assert!(document.retained_bytes() < recorder.total());
}
