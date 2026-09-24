// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Agent-IX
//! Peak heap while encoding, measured with a counting global allocator.
//!
//! This binary holds one test so no other test allocates concurrently; the
//! measurement is a deterministic count of bytes, not a timing.
//!
//! The worst shape for the member buffers is a flat top-level object of small
//! members: all of it is buffered until it closes, and the per-member offsets
//! are large next to members of a few bytes.

use std::alloc::{GlobalAlloc, Layout, System};
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};

use quire_canonical::{sha256, to_vec, Limits};

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

/// The peak heap added while `run` executes, above what was live before it.
fn peak_during(run: impl FnOnce()) -> usize {
    let baseline = CURRENT.load(Ordering::SeqCst);
    PEAK.store(baseline, Ordering::SeqCst);
    run();
    PEAK.load(Ordering::SeqCst) - baseline
}

/// FND-003: the measured bound the docs state. A 20,000-member flat object
/// peaked at 15.9x its canonical length with one `Vec` per member; one buffer
/// per frame plus 8-byte offsets keeps it under 4x.
#[test]
fn flat_object_peak_heap_stays_under_four_times_the_output() {
    let limits = Limits::new(u64::MAX, 8).expect("valid");
    let flat: BTreeMap<String, u32> = (0..20_000).map(|index| (format!("{index}"), 1)).collect();
    let nested: BTreeMap<String, BTreeMap<String, u32>> = (0..200)
        .map(|outer| {
            let inner = (0..100).map(|index| (format!("{index}"), 1)).collect();
            (format!("{outer}"), inner)
        })
        .collect();

    for (name, length, peak) in [
        (
            "flat",
            to_vec(&flat, limits).expect("encodes").len(),
            peak_during(|| {
                sha256(&flat, limits).expect("hashes");
            }),
        ),
        (
            "nested",
            to_vec(&nested, limits).expect("encodes").len(),
            peak_during(|| {
                sha256(&nested, limits).expect("hashes");
            }),
        ),
    ] {
        let ratio = peak as f64 / length as f64;
        println!("{name}: canonical {length} bytes, peak heap {peak} bytes, ratio {ratio:.2}");
        assert!(
            ratio < 4.0,
            "{name}: peak heap {ratio:.2}x the canonical length"
        );
    }
}
