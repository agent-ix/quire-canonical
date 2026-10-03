// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Agent-IX
//! `drop_value` frees what it drops, measured with a counting global
//! allocator (the `serde_json` feature).
//!
//! This binary holds one test so no other test allocates concurrently; the
//! measurement is a deterministic count of live bytes, not a timing.

#![cfg(feature = "serde_json")]

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};

use quire_canonical::drop_value;
use serde_json::{Map, Value};

const DEEP: usize = 100_000;

struct Counting;

static LIVE: AtomicUsize = AtomicUsize::new(0);

// SAFETY: every method forwards to `System` with the caller's arguments
// unchanged, so `System`'s guarantees hold; the counter is bookkeeping only.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // SAFETY: forwarded unchanged; the caller upholds `alloc`'s contract.
        let pointer = unsafe { System.alloc(layout) };
        if !pointer.is_null() {
            LIVE.fetch_add(layout.size(), Ordering::SeqCst);
        }
        pointer
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        // SAFETY: forwarded unchanged; the caller upholds `dealloc`'s contract.
        unsafe { System.dealloc(pointer, layout) };
        LIVE.fetch_sub(layout.size(), Ordering::SeqCst);
    }

    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        // SAFETY: forwarded unchanged; the caller upholds `realloc`'s contract.
        let moved = unsafe { System.realloc(pointer, layout, new_size) };
        if !moved.is_null() {
            LIVE.fetch_sub(layout.size(), Ordering::SeqCst);
            LIVE.fetch_add(new_size, Ordering::SeqCst);
        }
        moved
    }
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

/// `DEEP` nested arrays, around one array of a string and an object.
fn deep_array() -> Value {
    let mut leaf = Map::new();
    leaf.insert("leaf".to_owned(), Value::String("text".to_owned()));
    let mut value = Value::Array(vec![Value::String("text".to_owned()), Value::Object(leaf)]);
    for _ in 1..DEEP {
        value = Value::Array(vec![value]);
    }
    value
}

/// `DEEP` nested objects, `{"k":{"k":...{"k":null}}}`.
fn deep_object() -> Value {
    let mut value = Value::Null;
    for _ in 0..DEEP {
        let mut members = Map::new();
        members.insert("k".to_owned(), value);
        value = Value::Object(members);
    }
    value
}

/// Build a value, check it holds at least one `Value` of heap per level, drop
/// it with `drop_value`, and check the live heap is back where it started.
fn frees_everything(build: fn() -> Value) {
    let baseline = LIVE.load(Ordering::SeqCst);
    let value = build();
    let held = LIVE.load(Ordering::SeqCst).saturating_sub(baseline);
    assert!(
        held >= DEEP * std::mem::size_of::<Value>(),
        "the value holds its levels on the heap: {held} bytes"
    );
    drop_value(value);
    assert_eq!(
        LIVE.load(Ordering::SeqCst),
        baseline,
        "drop_value frees every byte the value held"
    );
}

/// `drop_value` frees every byte a 100,000-deep array and a 100,000-deep
/// object held: a body that leaked the value (`mem::forget`) would leave the
/// live heap above its baseline.
#[test]
fn drop_value_frees_every_byte_of_deep_values() {
    // One warm-up pass, so any one-time allocation made the first time these
    // types are used is not counted against the measured pass.
    drop_value(deep_array());
    drop_value(deep_object());
    frees_everything(deep_array);
    frees_everything(deep_object);
}
