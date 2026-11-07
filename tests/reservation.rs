// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Agent-IX
//! The writer's reservation hook: it is asked for exactly the heap the writer
//! requests from the allocator, a refusal leaves nothing behind, and exact
//! provision admits where one byte less refuses, for every shape of value.
//!
//! A counting global allocator measures the real heap, so the binary holds one
//! test and no other test allocates concurrently.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};

use quire_canonical::{
    encode_with_reservation, Encode, Error, FixedShape, Limits, ProtocolViolation, Reservation,
    ReservationDenied, Sink, Writer,
};
use serde::Serialize;

struct Counting;

/// Heap live now, and the bytes asked of the allocator by every allocation
/// and every reallocation (the new size of each).
static CURRENT: AtomicUsize = AtomicUsize::new(0);
static REQUESTED: AtomicUsize = AtomicUsize::new(0);

// SAFETY: every method forwards to `System` with the caller's arguments
// unchanged, so `System`'s guarantees hold; the counters are bookkeeping only.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // SAFETY: forwarded unchanged; the caller upholds `alloc`'s contract.
        let pointer = unsafe { System.alloc(layout) };
        if !pointer.is_null() {
            CURRENT.fetch_add(layout.size(), Ordering::SeqCst);
            REQUESTED.fetch_add(layout.size(), Ordering::SeqCst);
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
            CURRENT.fetch_add(new_size, Ordering::SeqCst);
            REQUESTED.fetch_add(new_size, Ordering::SeqCst);
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
    calls: usize,
}

impl Budget {
    fn new(limit: usize) -> Self {
        Self {
            limit,
            charged: 0,
            calls: 0,
        }
    }
}

impl Reservation for Budget {
    fn reserve(&mut self, bytes: usize) -> Result<(), ReservationDenied> {
        self.calls += 1;
        match self.charged.checked_add(bytes) {
            Some(total) if total <= self.limit => {
                self.charged = total;
                Ok(())
            }
            _ => Err(ReservationDenied),
        }
    }
}

/// A sink that keeps its bytes, or with `keep` unset allocates nothing, so
/// measured heap is the writer's alone.
struct Out {
    bytes: Vec<u8>,
    keep: bool,
}

impl Out {
    fn kept() -> Self {
        Self {
            bytes: Vec::new(),
            keep: true,
        }
    }

    fn discarded() -> Self {
        Self {
            bytes: Vec::new(),
            keep: false,
        }
    }
}

impl Sink for Out {
    fn write_bytes(&mut self, bytes: &[u8]) -> Result<(), Error> {
        if self.keep {
            self.bytes.extend_from_slice(bytes);
        }
        Ok(())
    }
}

const LIMITS: Limits = Limits::new(1 << 20);

/// What one metered run did.
struct Run {
    outcome: Result<u64, Error>,
    /// Heap live while the finished or refused writer still exists.
    live: usize,
    charged: usize,
    calls: usize,
    /// Bytes the allocator was asked for during the run.
    requested: usize,
}

fn run(
    sink: &mut Out,
    push: &impl Fn(&mut Writer<'_, Out>) -> Result<(), Error>,
    limit: usize,
) -> Run {
    let mut budget = Budget::new(limit);
    let asked = REQUESTED.load(Ordering::SeqCst);
    let (outcome, live) = {
        let mut writer = Writer::with_reservation(sink, LIMITS, &mut budget);
        let pushed = push(&mut writer);
        let live = CURRENT.load(Ordering::SeqCst);
        (pushed.and_then(|()| writer.finish()), live)
    };
    let requested = REQUESTED.load(Ordering::SeqCst) - asked;
    Run {
        outcome,
        live,
        charged: budget.charged,
        calls: budget.calls,
        requested,
    }
}

/// The reservation contract for one shape: the charge is what the allocator
/// was asked for, exact provision admits with the unmetered bytes, and every
/// smaller provision refuses with nothing in the sink and no heap kept.
/// Returns (total charged, hook calls).
fn check(shape: &str, push: impl Fn(&mut Writer<'_, Out>) -> Result<(), Error>) -> (usize, usize) {
    let expected = {
        let mut out = Out::kept();
        let mut writer = Writer::new(&mut out, LIMITS);
        push(&mut writer).expect("shape");
        writer.finish().expect("complete");
        out.bytes
    };

    let measured = run(&mut Out::discarded(), &push, usize::MAX);
    assert_eq!(
        measured.outcome.expect("admitted"),
        expected.len() as u64,
        "{shape}"
    );
    assert_eq!(measured.charged, measured.requested, "{shape}: charge");

    let mut out = Out::kept();
    let exact = run(&mut out, &push, measured.charged);
    assert_eq!(
        exact.outcome.expect("exact admits"),
        expected.len() as u64,
        "{shape}"
    );
    assert_eq!(out.bytes, expected, "{shape}");

    for limit in 0..measured.charged {
        let mut out = Out::kept();
        let before = CURRENT.load(Ordering::SeqCst);
        let refused = run(&mut out, &push, limit);
        assert!(
            matches!(
                refused.outcome,
                Err(Error::ReservationDenied { requested }) if requested > 0
            ),
            "{shape}, limit {limit}: {:?}",
            refused.outcome
        );
        assert!(
            out.bytes.is_empty(),
            "{shape}, limit {limit}: partial output"
        );
        assert_eq!(refused.live, before, "{shape}, limit {limit}: kept heap");
    }
    (measured.charged, measured.calls)
}

/// A member written out of order, holding a nested array and object.
fn member(writer: &mut Writer<'_, Out>, name: &str) -> Result<(), Error> {
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
    writer.end_object()
}

fn object_of(writer: &mut Writer<'_, Out>, names: &[String]) -> Result<(), Error> {
    writer.begin_object()?;
    for name in names.iter().rev() {
        member(writer, name)?;
    }
    writer.end_object()
}

fn array_of_objects(
    writer: &mut Writer<'_, Out>,
    names: &[String],
    count: usize,
) -> Result<(), Error> {
    writer.begin_array()?;
    for _ in 0..count {
        object_of(writer, names)?;
    }
    writer.end_array()
}

#[derive(Serialize, FixedShape)]
#[allow(dead_code)]
enum Tagged {
    Unit,
    Fields { b: u8, a: &'static str },
}

fn names(count: usize) -> Vec<String> {
    (0..count)
        .map(|index| format!("member{index:02}"))
        .collect()
}

#[test]
fn reservation_charges_exactly_the_writers_heap_and_a_refusal_leaves_nothing() {
    let names = names(10);

    // A top-level object, nested objects and arrays inside it.
    check("object", |writer| object_of(writer, &names));

    // Top-level containers leave nothing in the sink on refusal either.
    check("array", |writer| {
        writer.begin_array()?;
        writer.integer(1)?;
        writer.begin_array()?;
        writer.string("x")?;
        writer.end_array()?;
        writer.null()?;
        writer.end_array()
    });
    check("scalar", |writer| writer.string("a long enough string"));

    // A serde enum variant is a one-member object wrapper at the top level.
    check("enum wrapper", |writer| {
        writer.serialize(&Tagged::Fields { b: 1, a: "x" })
    });

    // Each object of a top-level array reuses the capacity the one before it
    // grew: three times the objects cost only a few more growths of the
    // output, not a regrowth of the member lists and walk stack per object.
    let few = names[..2].to_vec();
    let (_, calls_5) = check("five objects", |writer| array_of_objects(writer, &few, 5));
    let (_, calls_15) = check("fifteen objects", |writer| {
        array_of_objects(writer, &few, 15)
    });
    assert!(
        calls_15 <= calls_5 + 4,
        "{calls_15} growths for 15 objects, {calls_5} for 5"
    );

    // The crate's own Encode impls charge their stacks through the writer.
    let document = quire_canonical::read(br#"{"b":[1,{"d":null,"c":[true]}],"a":"x"}"#, 1 << 10)
        .expect("reads");
    check("document", |writer| document.encode_into(writer));
    check("node", |writer| document.root().encode_into(writer));
    #[cfg(feature = "serde_json")]
    {
        let value: serde_json::Value =
            serde_json::from_str(r#"{"b":[1,{"d":null,"c":[true]}],"a":"x"}"#).expect("parses");
        check("value", |writer| value.encode_into(writer));
    }

    // A caller's Encode impl charges its own scratch the same way.
    check("caller scratch", |writer| {
        let mut scratch: Vec<u64> = Vec::new();
        writer.reserve_list(&mut scratch, 100)?;
        scratch.resize(100, 7);
        writer.reserve(16)?;
        let _scratch: Vec<u8> = Vec::with_capacity(16);
        writer.begin_array()?;
        writer.end_array()
    });

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
    // allocation is asked for too, at the capacity actually allocated.
    let short = ["ab".to_owned()];
    let duplicate = |writer: &mut Writer<'_, Out>| {
        writer.begin_object()?;
        for name in &short {
            writer.name(name)?;
            writer.null()?;
            writer.name(name)?;
            writer.null()?;
        }
        writer.end_object()
    };
    let measured = run(&mut Out::discarded(), &duplicate, usize::MAX);
    assert!(
        matches!(measured.outcome, Err(Error::DuplicateMemberName { ref name }) if name == "ab"),
        "{:?}",
        measured.outcome
    );
    assert_eq!(measured.charged, measured.requested);
    let denied = run(&mut Out::discarded(), &duplicate, measured.charged - 1);
    assert!(
        matches!(
            denied.outcome,
            Err(Error::ReservationDenied { requested: 4 })
        ),
        "{:?}",
        denied.outcome
    );

    // The one-shot entry reaches the same hook.
    let mut bytes = Vec::new();
    let mut budget = Budget::new(16);
    let outcome = encode_with_reservation(&mut bytes, &document, LIMITS, &mut budget);
    assert!(matches!(outcome, Err(Error::ReservationDenied { .. })));
    assert!(bytes.is_empty());
}
