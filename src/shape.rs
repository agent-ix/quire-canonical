// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Agent-IX
//! [`FixedShape`]: the types that may take the serde encoding path.

use alloc::borrow::{Cow, ToOwned};
use alloc::boxed::Box;
use alloc::collections::{BTreeMap, BTreeSet};
use alloc::string::String;
use alloc::vec::Vec;

use serde::Serialize;

use crate::Sha256Digest;

/// A type whose canonical JSON nests to a depth fixed by the type itself, so
/// serde's recursive `Serialize` is safe to drive the encoder with.
///
/// [`FixedShape::DEPTH`] is the number of arrays and objects the type's JSON
/// nests. It is never a limit: no encoding is refused on depth. Its job is to
/// make a recursive type fail to compile, and it does that only when it is
/// computed from the `DEPTH` of every field's type: then a type that
/// contains itself refers to its own `DEPTH`, the encoder evaluates `DEPTH`
/// at compile time, and rustc rejects the cycle (E0391).
///
/// Derive it. `#[derive(FixedShape)]` computes `DEPTH` with [`nest`] and
/// [`deepest`] over every field's `DEPTH`, so a recursive type cannot derive
/// it:
///
/// ```compile_fail,E0391
/// use quire_canonical::{to_vec, FixedShape, Limits};
///
/// #[derive(serde::Serialize, FixedShape)]
/// struct List { next: Option<Box<List>> }
///
/// let _ = to_vec(&List { next: None }, Limits::new(64));
/// ```
///
/// A hand-written impl must follow the same rule: `DEPTH` is `nest` (or
/// `deepest`, for alternatives) over the `DEPTH` of every field's type.
/// Written that way, a recursive type is refused the same way:
///
/// ```compile_fail,E0391
/// use quire_canonical::{nest, to_vec, FixedShape, Limits};
///
/// #[derive(serde::Serialize)]
/// struct List { next: Option<Box<List>> }
///
/// impl FixedShape for List {
///     const DEPTH: usize = nest(&[<Option<Box<List>>>::DEPTH]);
/// }
///
/// let _ = to_vec(&List { next: None }, Limits::new(64));
/// ```
///
/// A hand-written literal `DEPTH` defeats the check. The compiler cannot
/// tell that a type with `const DEPTH: usize = 1` contains itself, or holds
/// a recursive foreign type such as `serde_json::Value`, and such a value
/// then recurses natively through serde and can overflow the stack. Only
/// write a literal for a type with no fields to name, such as one with a
/// hand-written `Serialize` that emits a scalar.
///
/// A type whose depth follows its input implements [`crate::Encode`] instead,
/// pushing [`crate::Writer`] events from an explicit stack.
pub trait FixedShape: Serialize {
    /// How many arrays and objects deep the type's JSON nests.
    const DEPTH: usize;
}

/// The deepest of the given depths, or `0` for none: the `DEPTH` of a type
/// that is one of several alternatives, such as an enum.
#[must_use]
pub const fn deepest(depths: &[usize]) -> usize {
    let mut deepest = 0;
    let mut index = 0;
    while index < depths.len() {
        if depths[index] > deepest {
            deepest = depths[index];
        }
        index += 1;
    }
    deepest
}

/// The `DEPTH` of one array or object around values of the given depths:
/// one more than the deepest of them.
#[must_use]
pub const fn nest(depths: &[usize]) -> usize {
    deepest(depths).saturating_add(1)
}

macro_rules! scalar_shapes {
    ($($scalar:ty),* $(,)?) => {
        $(impl FixedShape for $scalar {
            const DEPTH: usize = 0;
        })*
    };
}

scalar_shapes!(
    bool,
    i8,
    i16,
    i32,
    i64,
    i128,
    isize,
    u8,
    u16,
    u32,
    u64,
    u128,
    usize,
    f32,
    f64,
    char,
    str,
    String,
    (),
);

impl<T: FixedShape + ?Sized> FixedShape for &T {
    const DEPTH: usize = T::DEPTH;
}

impl<T: FixedShape + ?Sized> FixedShape for &mut T {
    const DEPTH: usize = T::DEPTH;
}

impl<T: FixedShape + ?Sized> FixedShape for Box<T> {
    const DEPTH: usize = T::DEPTH;
}

impl<T: FixedShape + ToOwned + ?Sized> FixedShape for Cow<'_, T> {
    const DEPTH: usize = T::DEPTH;
}

impl<T: FixedShape> FixedShape for Option<T> {
    const DEPTH: usize = T::DEPTH;
}

impl<T: FixedShape> FixedShape for [T] {
    const DEPTH: usize = nest(&[T::DEPTH]);
}

// serde implements `Serialize` for arrays of up to 32 elements.
macro_rules! array_shapes {
    ($($length:literal),* $(,)?) => {
        $(impl<T: FixedShape> FixedShape for [T; $length] {
            const DEPTH: usize = nest(&[T::DEPTH]);
        })*
    };
}

array_shapes!(
    0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24, 25,
    26, 27, 28, 29, 30, 31, 32,
);

impl<T: FixedShape> FixedShape for Vec<T> {
    const DEPTH: usize = nest(&[T::DEPTH]);
}

impl<T: FixedShape> FixedShape for BTreeSet<T> {
    const DEPTH: usize = nest(&[T::DEPTH]);
}

impl<K: FixedShape, V: FixedShape> FixedShape for BTreeMap<K, V> {
    const DEPTH: usize = nest(&[V::DEPTH]);
}

#[cfg(feature = "std")]
impl<T: FixedShape, H> FixedShape for std::collections::HashSet<T, H> {
    const DEPTH: usize = nest(&[T::DEPTH]);
}

#[cfg(feature = "std")]
impl<K: FixedShape, V: FixedShape, H> FixedShape for std::collections::HashMap<K, V, H> {
    const DEPTH: usize = nest(&[V::DEPTH]);
}

macro_rules! tuple_shapes {
    ($(($($name:ident),+)),* $(,)?) => {
        $(impl<$($name: FixedShape),+> FixedShape for ($($name,)+) {
            const DEPTH: usize = nest(&[$($name::DEPTH),+]);
        })*
    };
}

tuple_shapes!(
    (A),
    (A, B),
    (A, B, C),
    (A, B, C, D),
    (A, B, C, D, E),
    (A, B, C, D, E, F),
    (A, B, C, D, E, F, G),
    (A, B, C, D, E, F, G, H),
);

impl FixedShape for Sha256Digest {
    const DEPTH: usize = nest(&[u8::DEPTH]);
}
