// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Agent-IX
//! The counting pass of `read_exact_with_reservation` holds no heap but the
//! open-container stack, whatever the input's size; pass 2 asks before each
//! allocation; and the charge bounds the read's peak heap.
//!
//! A counting global allocator measures the real heap, so the binary holds one
//! test and no other test allocates concurrently.
#![cfg(target_pointer_width = "64")]

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};

use quire_canonical::{read_exact_with_reservation, Reservation, ReservationDenied};

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

/// Records every request with the heap live, and the most heap that had
/// been live, above the baseline when it was asked.
struct Recorder {
    baseline: usize,
    requests: Vec<usize>,
    live_when_asked: Vec<usize>,
    peak_when_asked: Vec<usize>,
}

impl Recorder {
    fn new() -> Self {
        let requests = Vec::with_capacity(64);
        let live_when_asked = Vec::with_capacity(64);
        let peak_when_asked = Vec::with_capacity(64);
        let baseline = CURRENT.load(Ordering::SeqCst);
        PEAK.store(baseline, Ordering::SeqCst);
        Self {
            baseline,
            requests,
            live_when_asked,
            peak_when_asked,
        }
    }
}

impl Reservation for Recorder {
    fn reserve(&mut self, bytes: usize) -> Result<(), ReservationDenied> {
        self.live_when_asked
            .push(CURRENT.load(Ordering::SeqCst) - self.baseline);
        self.peak_when_asked
            .push(PEAK.load(Ordering::SeqCst) - self.baseline);
        self.requests.push(bytes);
        Ok(())
    }
}

fn flat_array(width: usize) -> String {
    let mut text = String::from("[");
    for index in 0..width {
        if index > 0 {
            text.push(',');
        }
        text.push_str(&format!("{}", index % 10));
    }
    text.push(']');
    text
}

#[test]
fn the_counting_pass_holds_nothing_but_the_stack() {
    // A scalar: no stack, so pass 1 asks for and allocates nothing, and the
    // first request is pass 2's text, with nothing live.
    let mut recorder = Recorder::new();
    let document = read_exact_with_reservation(b"\"abc\"", u64::MAX, &mut recorder).expect("reads");
    assert_eq!(recorder.requests, [3, 8, 1]);
    assert_eq!(recorder.peak_when_asked[0], 0);
    drop(document);

    // A flat array of any width: pass 1 asks once, for the stack's first
    // four entries, and that block is all it ever holds.
    for width in [10, 1_000, 100_000] {
        let input = flat_array(width);
        let mut recorder = Recorder::new();
        let document =
            read_exact_with_reservation(input.as_bytes(), u64::MAX, &mut recorder).expect("reads");
        assert_eq!(
            recorder.requests,
            [64, width, 8 * (width + 1), width + 1, 4 * width, 8 * width],
            "width {width}"
        );
        assert_eq!(recorder.live_when_asked[1], 64, "width {width}");
        assert_eq!(recorder.peak_when_asked[1], 64, "width {width}");
        // Each request comes before its allocation, and the total bounds
        // the peak.
        let mut charged = 0;
        for (index, bytes) in recorder.requests.iter().enumerate() {
            assert!(recorder.live_when_asked[index] <= charged, "width {width}");
            charged += bytes;
        }
        let peak = PEAK.load(Ordering::SeqCst) - recorder.baseline;
        assert!(peak <= charged, "width {width}: peak {peak} over {charged}");
        drop(document);
    }
}
