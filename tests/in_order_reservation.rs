// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Agent-IX
//! The in-order writer's heap, against a counting allocator: the reservation
//! is asked for exactly the bytes the writer requests from the allocator, a
//! denied growth allocates nothing and leaves no heap behind, and the charge
//! depends on the value's depth and name lengths, never on its length.
//!
//! A counting global allocator measures the real heap, so the binary holds one
//! test and no other test allocates concurrently.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};

use quire_canonical::{Error, Limits, Reservation, ReservationDenied, Sink, Writer};

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

/// A budget: admits growth while the total stays within `limit`, and counts
/// the calls it gets.
struct Budget {
    limit: usize,
    charged: usize,
    calls: usize,
    denied_at: Option<usize>,
}

impl Reservation for Budget {
    fn reserve(&mut self, bytes: usize) -> Result<(), ReservationDenied> {
        self.calls += 1;
        match self.charged.checked_add(bytes) {
            Some(total) if total <= self.limit => {
                self.charged = total;
                Ok(())
            }
            _ => {
                self.denied_at.get_or_insert(self.calls);
                Err(ReservationDenied)
            }
        }
    }
}

/// A sink that counts its bytes and allocates nothing, so measured heap is
/// the writer's alone.
struct Counted(u64);

impl Sink for Counted {
    fn write_bytes(&mut self, bytes: &[u8]) -> Result<(), Error> {
        self.0 += bytes.len() as u64;
        Ok(())
    }
}

const LIMITS: Limits = Limits::new(1 << 40);

/// What one metered in-order run did.
struct Run {
    outcome: Result<u64, Error>,
    /// Heap live while the finished or refused writer still exists, above
    /// what was live before it.
    kept: isize,
    charged: usize,
    calls: usize,
    denied_at: Option<usize>,
    /// Bytes the allocator was asked for during the run.
    requested: usize,
    /// Bytes the sink received.
    sunk: u64,
}

fn run(push: &impl Fn(&mut Writer<'_, Counted>) -> Result<(), Error>, limit: usize) -> Run {
    let mut budget = Budget {
        limit,
        charged: 0,
        calls: 0,
        denied_at: None,
    };
    let mut sink = Counted(0);
    let before = CURRENT.load(Ordering::SeqCst);
    let asked = REQUESTED.load(Ordering::SeqCst);
    let (outcome, live) = {
        let mut writer = Writer::in_order_with_reservation(&mut sink, LIMITS, &mut budget);
        let pushed = push(&mut writer);
        let live = CURRENT.load(Ordering::SeqCst);
        (pushed.and_then(|()| writer.finish()), live)
    };
    let requested = REQUESTED.load(Ordering::SeqCst) - asked;
    Run {
        outcome,
        kept: live as isize - before as isize,
        charged: budget.charged,
        calls: budget.calls,
        denied_at: budget.denied_at,
        requested,
        sunk: sink.0,
    }
}

/// The charge for one shape is what the allocator was asked for. With
/// `every_limit`, each smaller budget is refused at its first denied growth,
/// with nothing asked after it, nothing allocated for it and no heap kept.
/// Returns the charge.
fn check(
    shape: &str,
    every_limit: bool,
    push: impl Fn(&mut Writer<'_, Counted>) -> Result<(), Error>,
) -> usize {
    let measured = run(&push, usize::MAX);
    let length = measured.outcome.expect("admitted");
    assert_eq!(
        measured.sunk, length,
        "{shape}: every byte reached the sink"
    );
    assert_eq!(measured.charged, measured.requested, "{shape}: charge");
    if every_limit {
        for limit in 0..measured.charged {
            let refused = run(&push, limit);
            assert!(
                matches!(
                    refused.outcome,
                    Err(Error::ReservationDenied { requested }) if requested > 0
                ),
                "{shape}, limit {limit}: {:?}",
                refused.outcome
            );
            assert_eq!(
                refused.denied_at,
                Some(refused.calls),
                "{shape}, limit {limit}: asked again after the denial"
            );
            assert_eq!(
                refused.charged, refused.requested,
                "{shape}, limit {limit}: allocated past the charge"
            );
            assert_eq!(refused.kept, 0, "{shape}, limit {limit}: kept heap");
            assert!(
                refused.sunk < length,
                "{shape}, limit {limit}: whole output"
            );
        }
    }
    measured.charged
}

/// A flat object of members named `names`, in order. The names are built
/// before the run, so the run allocates nothing of its own.
fn flat(writer: &mut Writer<'_, Counted>, names: &[String]) -> Result<(), Error> {
    writer.begin_object()?;
    for (index, name) in names.iter().enumerate() {
        writer.name(name)?;
        writer.integer(index as i128)?;
    }
    writer.end_object()
}

/// `{"k":{"k":...}}`, `depth` objects deep, around `leaf`.
fn chain(
    writer: &mut Writer<'_, Counted>,
    depth: usize,
    leaf: &impl Fn(&mut Writer<'_, Counted>) -> Result<(), Error>,
) -> Result<(), Error> {
    for _ in 0..depth {
        writer.begin_object()?;
        writer.name("k")?;
    }
    leaf(writer)?;
    for _ in 0..depth {
        writer.end_object()?;
    }
    Ok(())
}

#[test]
fn in_order_heap_is_charged_exactly_and_proportional_to_depth() {
    // Small shapes: exact charge, and a denial at every possible point.
    check("scalar", true, |writer| {
        writer.string("a long enough string")
    });
    let names: Vec<String> = (0..100_000).map(|index| format!("m{index:07}")).collect();
    check("flat", true, |writer| flat(writer, &names[..10]));
    check("chain", true, |writer| {
        chain(writer, 40, &|writer| writer.null())
    });
    check("mixed", true, |writer| {
        writer.begin_array()?;
        for _ in 0..3 {
            writer.begin_object()?;
            writer.name("alpha")?;
            writer.begin_array()?;
            writer.begin_object()?;
            writer.name("beta")?;
            writer.null()?;
            writer.name("gamma")?;
            writer.bool(true)?;
            writer.end_object()?;
            writer.end_array()?;
            writer.name("delta")?;
            writer.string("x")?;
            writer.end_object()?;
        }
        writer.end_array()
    });

    // A flat object costs the same whatever its length: the container stack
    // (grown once, to 4 entries of 2 bytes), the object records (4 of 16 bytes
    // on a 64-bit target) and one previous name (8 bytes), reused by every
    // member. 80 bytes on a 64-bit target, for 16 members or 100,000.
    let flat_small = check("flat 16", false, |writer| flat(writer, &names[..16]));
    let flat_large = check("flat 100000", false, |writer| flat(writer, &names));
    assert_eq!(
        flat_small, flat_large,
        "flat object charge grew with length"
    );
    #[cfg(target_pointer_width = "64")]
    assert_eq!(flat_large, 80);

    // A deep chain costs per level 2 bytes of stack, 16 of object record and
    // 1 of name. Each list grows by doubling, so its charges sum to under 4
    // times its final length: at most 76 bytes per level, for any length.
    for depth in [100, 1000, 10_000] {
        let charge = check("chain", false, |writer| {
            chain(writer, depth, &|writer| writer.null())
        });
        assert!(
            charge <= 76 * (depth + 4),
            "depth {depth}: {charge} bytes charged"
        );
        // The same depth around a leaf of 1 MiB charges the same.
        let text = "x".repeat(1 << 20);
        let long = check("chain with long leaf", false, |writer| {
            chain(writer, depth, &|writer| writer.string(&text))
        });
        assert_eq!(long, charge, "depth {depth}: charge grew with length");
        // And around a long array of scalars: one more stack entry, inside
        // the capacity the chain already grew.
        let wide = check("chain with wide leaf", false, |writer| {
            chain(writer, depth, &|writer| {
                writer.begin_array()?;
                for index in 0..50_000 {
                    writer.integer(index)?;
                }
                writer.end_array()
            })
        });
        assert_eq!(wide, charge, "depth {depth}: charge grew with length");
    }
}
