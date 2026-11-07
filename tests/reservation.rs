// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Agent-IX
//! The writer's reservation hook: it is asked for exactly the heap the writer
//! allocates, a refusal leaves nothing behind, and exact provision admits
//! where one byte less refuses.
//!
//! A counting global allocator measures the real heap, so the binary holds one
//! test and no other test allocates concurrently.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};

use quire_canonical::{
    encode_with_reservation, to_vec, Error, Limits, ProtocolViolation, Reservation,
    ReservationDenied, Sink, Writer,
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

/// A budget: admits growth while the total stays within `limit`.
struct Budget {
    limit: usize,
    charged: usize,
}

impl Budget {
    fn new(limit: usize) -> Self {
        Self { limit, charged: 0 }
    }
}

impl Reservation for Budget {
    fn reserve(&mut self, bytes: usize) -> Result<(), ReservationDenied> {
        match self.charged.checked_add(bytes) {
            Some(total) if total <= self.limit => {
                self.charged = total;
                Ok(())
            }
            _ => Err(ReservationDenied),
        }
    }
}

/// A sink that allocates nothing, so measured heap is the writer's alone.
struct Discard;

impl Sink for Discard {
    fn write_bytes(&mut self, _bytes: &[u8]) -> Result<(), Error> {
        Ok(())
    }
}

const LIMITS: Limits = Limits::new(1 << 20);

/// An out-of-order object tree with enough members and nesting to grow every
/// list the writer holds more than once.
fn push_tree<S: Sink + ?Sized>(
    writer: &mut Writer<'_, S>,
    names: &[String],
    duplicate: bool,
) -> Result<(), Error> {
    writer.begin_object()?;
    for name in names.iter().rev() {
        writer.name(name)?;
        writer.begin_object()?;
        writer.name("z")?;
        writer.begin_array()?;
        writer.integer(1)?;
        writer.begin_object()?;
        writer.name("y")?;
        writer.null()?;
        writer.name("x")?;
        writer.string("text")?;
        writer.end_object()?;
        writer.end_array()?;
        writer.name("a")?;
        writer.bool(true)?;
        writer.end_object()?;
    }
    if duplicate {
        writer.name(&names[0])?;
        writer.null()?;
    }
    writer.end_object()?;
    Ok(())
}

fn names() -> Vec<String> {
    (0..40).map(|index| format!("member{index:02}")).collect()
}

/// Run the tree under `limit`, into `sink`, returning the writer's outcome
/// and the heap still live while the (refused or finished) writer exists.
fn run<S: Sink + ?Sized>(
    sink: &mut S,
    names: &[String],
    duplicate: bool,
    limit: usize,
) -> (Result<u64, Error>, usize, usize) {
    let mut budget = Budget::new(limit);
    let outcome = {
        let mut writer = Writer::with_reservation(sink, LIMITS, &mut budget);
        let pushed = push_tree(&mut writer, names, duplicate);
        let live = CURRENT.load(Ordering::SeqCst);
        let outcome = pushed.and_then(|()| writer.finish());
        (outcome, live)
    };
    (outcome.0, outcome.1, budget.charged)
}

#[test]
fn reservation_charges_exactly_the_writers_heap_and_a_refusal_leaves_nothing() {
    let names = names();
    let expected = {
        let mut bytes = Vec::new();
        let mut writer = Writer::new(&mut bytes, LIMITS);
        push_tree(&mut writer, &names, false).expect("tree");
        writer.finish().expect("complete");
        bytes
    };

    // What the writer is charged is what it allocates: the charge in total
    // equals the peak heap the allocator saw.
    let baseline = CURRENT.load(Ordering::SeqCst);
    PEAK.store(baseline, Ordering::SeqCst);
    let (outcome, _, total) = run(&mut Discard, &names, false, usize::MAX);
    let peak = PEAK.load(Ordering::SeqCst) - baseline;
    assert_eq!(outcome.expect("admitted"), expected.len() as u64);
    assert!(total > expected.len(), "buffering costs more than the text");
    assert_eq!(total, peak);

    // Exact provision admits and writes the same bytes as an unmetered writer.
    let mut bytes = Vec::new();
    let (outcome, _, charged) = run(&mut bytes, &names, false, total);
    assert_eq!(outcome.expect("exact admits"), expected.len() as u64);
    assert_eq!(charged, total);
    assert_eq!(bytes, expected);
    assert_eq!(bytes, to_vec(&PlainTree(&names), LIMITS).expect("encodes"));

    // One byte under refuses, with nothing in the sink and nothing retained.
    // Every shorter provision does the same, whichever growth it falls at.
    for limit in 0..total {
        let mut bytes = Vec::new();
        let before = CURRENT.load(Ordering::SeqCst);
        let (outcome, live, _) = run(&mut bytes, &names, false, limit);
        assert!(
            matches!(outcome, Err(Error::ReservationDenied { requested }) if requested > 0),
            "limit {limit}: {outcome:?}"
        );
        assert!(bytes.is_empty(), "limit {limit}: partial output");
        assert_eq!(live, before, "limit {limit}: the refused writer kept heap");
    }

    // After a refusal the writer refuses every further event.
    let mut bytes = Vec::new();
    let mut budget = Budget::new(0);
    let mut writer = Writer::with_reservation(&mut bytes, LIMITS, &mut budget);
    assert!(matches!(
        writer.begin_object(),
        Err(Error::ReservationDenied { .. })
    ));
    assert!(matches!(
        writer.null(),
        Err(Error::Protocol(ProtocolViolation::AfterRefusal))
    ));

    // The duplicate-name refusal allocates the name it reports, so that
    // allocation is asked for too.
    let (outcome, _, total) = run(&mut Discard, &names, true, usize::MAX);
    assert!(
        matches!(outcome, Err(Error::DuplicateMemberName { ref name }) if *name == names[0]),
        "{outcome:?}"
    );
    let (outcome, _, _) = run(&mut Discard, &names, true, total - 1);
    assert!(
        matches!(outcome, Err(Error::ReservationDenied { requested }) if requested == names[0].len()),
        "{outcome:?}"
    );

    encode_with_reservation_is_denied_through_the_one_shot_entry();
}

/// The same tree through the public one-shot entry, to show the hook reaches
/// `encode_with_reservation` callers.
struct PlainTree<'a>(&'a [String]);

impl quire_canonical::Encode for PlainTree<'_> {
    fn encode_into<S: Sink + ?Sized>(&self, writer: &mut Writer<'_, S>) -> Result<(), Error> {
        push_tree(writer, self.0, false)
    }
}

fn encode_with_reservation_is_denied_through_the_one_shot_entry() {
    let names = names();
    let mut bytes = Vec::new();
    let mut budget = Budget::new(16);
    let outcome = encode_with_reservation(&mut bytes, &PlainTree(&names), LIMITS, &mut budget);
    assert!(matches!(outcome, Err(Error::ReservationDenied { .. })));
    assert!(bytes.is_empty());
}
