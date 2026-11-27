// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Agent-IX
//! `Document::extract_with_reservation` against the real heap: each request
//! comes before its allocation, the heap held when a request is made is
//! exactly the earlier requests, the result holds exactly what was asked, and
//! a denial leaves nothing behind.
//!
//! A counting global allocator measures the real heap, so the binary holds one
//! test and no other test allocates concurrently.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};

use quire_canonical::{read, ExtractError, Reservation, ReservationDenied};

struct Counting;

static CURRENT: AtomicUsize = AtomicUsize::new(0);

// SAFETY: every method forwards to `System` with the caller's arguments
// unchanged, so `System`'s guarantees hold; the counter is bookkeeping only.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // SAFETY: forwarded unchanged; the caller upholds `alloc`'s contract.
        let pointer = unsafe { System.alloc(layout) };
        if !pointer.is_null() {
            CURRENT.fetch_add(layout.size(), Ordering::SeqCst);
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
        }
        moved
    }
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

/// Records every request and the heap live above `baseline` when it was
/// made, and denies the request at `deny`. Its lists are allocated up front.
struct Recorder {
    baseline: usize,
    deny: Option<usize>,
    requests: Vec<usize>,
    live_when_asked: Vec<usize>,
}

impl Recorder {
    fn new(deny: Option<usize>) -> Self {
        let requests = Vec::with_capacity(16);
        let live_when_asked = Vec::with_capacity(16);
        Self {
            baseline: CURRENT.load(Ordering::SeqCst),
            deny,
            requests,
            live_when_asked,
        }
    }
}

impl Reservation for Recorder {
    fn reserve(&mut self, bytes: usize) -> Result<(), ReservationDenied> {
        let live = CURRENT.load(Ordering::SeqCst) - self.baseline;
        self.live_when_asked.push(live);
        self.requests.push(bytes);
        if self.deny == Some(self.requests.len() - 1) {
            return Err(ReservationDenied);
        }
        Ok(())
    }
}

#[test]
fn every_allocation_follows_its_request() {
    let text = br#"{"keep":{"a":["x",1.25,true,{"b":null}],"c":"yz"},"rest":[1,2,3,"long unselected text"]}"#;
    let source = read(text, 1024).expect("reads");
    let keep = source.root().get("keep").expect("member").id();
    let roots = [keep, keep];

    let mut recorder = Recorder::new(None);
    let (extract, ids) = source
        .extract_with_reservation(&roots, &mut recorder)
        .expect("extracts");
    let held = CURRENT.load(Ordering::SeqCst) - recorder.baseline;
    // Six lists, each asked once, and the heap when each was asked is the
    // sum of the earlier requests: nothing was allocated before its request
    // or outside one.
    assert_eq!(recorder.requests.len(), 6);
    let mut earlier = 0;
    for (request, live) in recorder.requests.iter().zip(&recorder.live_when_asked) {
        assert_eq!(*live, earlier);
        earlier += request;
    }
    // What the result holds is exactly what was asked.
    assert_eq!(held, earlier);
    assert_eq!(
        extract.retained_bytes() + ids.capacity() * size_of::<usize>(),
        earlier
    );
    let requests = recorder.requests.clone();
    drop((extract, ids));

    for deny in 0..requests.len() {
        let mut recorder = Recorder::new(Some(deny));
        let refused = source.extract_with_reservation(&roots, &mut recorder);
        assert_eq!(
            refused.err(),
            Some(ExtractError::ReservationDenied {
                requested: requests[deny]
            })
        );
        assert_eq!(recorder.requests, requests[..=deny]);
        // Everything allocated before the denial is gone.
        assert_eq!(CURRENT.load(Ordering::SeqCst), recorder.baseline);
    }
}
