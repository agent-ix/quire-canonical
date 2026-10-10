// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Agent-IX
//! Peak heap while encoding and reading, measured with a counting global
//! allocator.
//!
//! This binary holds one test so no other test allocates concurrently; the
//! measurement is a deterministic count of bytes, not a timing.

use std::alloc::{GlobalAlloc, Layout, System};
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};

use quire_canonical::{read, sha256, to_vec, Limits, Writer};

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

/// A `{"k":{"k":...null...}}` chain `levels` deep, pushed as events, hashed.
fn deep_object_chain(levels: usize, limits: Limits) -> u64 {
    let mut hasher = <sha2::Sha256 as sha2::Digest>::new();
    let mut writer = Writer::new(&mut hasher, limits);
    for _ in 0..levels {
        writer.begin_object().expect("open");
        writer.name("k").expect("name");
    }
    writer.null().expect("leaf");
    for _ in 0..levels {
        writer.end_object().expect("close");
    }
    writer.finish().expect("complete")
}

/// `ratio` of `peak` to `length`, printed for the record.
fn ratio(name: &str, length: usize, peak: usize) -> f64 {
    let ratio = peak as f64 / length as f64;
    println!("{name}: {length} bytes, peak heap {peak} bytes, ratio {ratio:.2}");
    ratio
}

/// The measured bounds the writer and reader docs state, per shape. One test,
/// so nothing else in this binary allocates while it measures.
///
/// * A flat object of small members, the worst case for member offsets,
///   stays under 4x its canonical length, and so does an object of objects.
/// * A deep object chain, the worst case per byte (a stack entry, a frame
///   and a closed-object record for every 6 bytes of `{"k":` and `}`), stays
///   under the bound the writer docs give for it.
/// * The reader stays under the multiple of its input the reader docs give,
///   for deep arrays, deep objects and flat arrays of one-byte numbers.
#[test]
fn peak_heap_stays_within_the_documented_bounds() {
    let limits = Limits::new(u64::MAX);
    // The payload is caller-owned and allocated before measurement. Names
    // have fixed width; their ascending decimal order is canonical order.
    let payload = "x".repeat(4096);
    let mut previous_peak = None;
    for count in [128, 8192] {
        let mut length = 0;
        let peak = peak_during(|| {
            let mut sink = <sha2::Sha256 as sha2::Digest>::new();
            let mut writer = Writer::new(&mut sink, limits);
            writer.begin_ordered_object().expect("ordered root");
            for index in 0..count {
                writer.name(&format!("{index:08}")).expect("ordered name");
                writer.string(&payload).expect("streamed payload");
            }
            writer.end_object().expect("close root");
            length = writer.finish().expect("complete");
        });
        assert_eq!(length, 1 + count * (4096 + 14));
        println!("ordered root {count}: {length} bytes, peak heap {peak} bytes");
        assert!(peak < 4096, "root retained already-streamed values: {peak}");
        if let Some(previous) = previous_peak {
            assert_eq!(peak, previous, "storage scaled with total streamed bytes");
        }
        previous_peak = Some(peak);
    }
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
        let ratio = ratio(name, length, peak);
        assert!(
            ratio < 4.0,
            "{name}: peak heap {ratio:.2}x the canonical length"
        );
    }

    for levels in [1_000, 10_000, 100_000] {
        let mut length = 0;
        let peak = peak_during(|| length = deep_object_chain(levels, limits));
        let length = usize::try_from(length).expect("fits");
        let ratio = ratio(&format!("deep object chain {levels}"), length, peak);
        assert!(
            ratio < DEEP_OBJECT_BOUND,
            "deep chain {levels}: peak heap {ratio:.2}x the canonical length"
        );
    }

    for size in [1_000, 10_000, 100_000] {
        let deep_arrays = format!("{}{}", "[".repeat(size), "]".repeat(size));
        let deep_objects = format!("{}null{}", r#"{"k":"#.repeat(size), "}".repeat(size));
        let flat_numbers = format!("[{}1]", "1,".repeat(size));
        for (name, input) in [
            ("deep arrays", deep_arrays),
            ("deep objects", deep_objects),
            ("flat numbers", flat_numbers),
        ] {
            let peak = peak_during(|| {
                read(input.as_bytes(), u64::MAX).expect("reads");
            });
            let ratio = ratio(&format!("read {name} {size}"), input.len(), peak);
            assert!(
                ratio < READ_BOUND,
                "read {name} {size}: peak heap {ratio:.2}x the input length"
            );
        }
    }
}

/// The deep-object bound the writer docs state.
const DEEP_OBJECT_BOUND: f64 = 32.0;
/// The input multiple the reader docs state.
const READ_BOUND: f64 = 64.0;
