// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Agent-IX
//! One streaming RFC 8785 (JSON Canonicalization Scheme) encoder that hashes
//! as it encodes, and the one shared reader of untrusted JSON.
//!
//! A value is encoded to its RFC 8785 canonical text and fed, in order, to a
//! [`Sink`]: a SHA-256 hasher, a byte vector, an `std::io::Write` (with the
//! `std` feature), or a pair of them. There is no intermediate `String` of
//! the whole text.
//!
//! # Depth
//!
//! Nothing in this crate recurses in proportion to its input, and nothing
//! bounds depth. The only limits are byte limits: [`Limits::max_bytes`] on
//! canonical output and the input byte limit of [`read`]. Every heap stack
//! the crate grows is bounded by one of them. A value reaches the encoder by
//! one of three paths:
//!
//! * [`Writer`], the push-event API: the caller pushes `begin_array`, `name`,
//!   `string`, `end_object` and so on from its own explicit stack. This is
//!   the path for data whose depth follows its input.
//! * [`Document`], the arena tree [`read`] produces, encodes itself through
//!   the [`Writer`] from an explicit heap stack. With the `serde_json`
//!   feature, so does `serde_json::Value`, and `drop_value` drops one
//!   without recursing.
//! * A [`FixedShape`] type encodes through its serde `Serialize`. serde
//!   recurses once per nesting level, so only types whose depth is fixed by
//!   their schema may take this path. `#[derive(FixedShape)]` computes
//!   [`FixedShape::DEPTH`] from every field's `DEPTH`, so a recursive type
//!   refers to its own `DEPTH` and rustc refuses the cycle at compile time.
//!   A hand-written impl gets that check only if it computes `DEPTH` the same
//!   way; a literal `DEPTH` defeats it (see [`FixedShape`]).
//!
//! [`encode`], [`to_vec`], [`sha256`] and [`sha256_with_domain`] accept any
//! [`Encode`] value: a [`FixedShape`] type, a [`Document`] or [`NodeRef`], a
//! `serde_json::Value` (with the `serde_json` feature), or a caller's own
//! event source.
//!
//! # Encoding rules
//!
//! * Member names are ordered by UTF-16 code unit (RFC 8785 §3.2.3) by the
//!   encoder itself, so the bytes do not depend on how a map type is backed.
//! * Numbers use ECMAScript `Number::toString` (§3.2.2.3). A Rust integer
//!   type (`i8`..`i128`, `u8`..`u128`) is encoded by its IEEE 754 double value
//!   and refused, naming the value, if its magnitude exceeds `2^53`
//!   (9007199254740992) — the largest integer every smaller one, and it,
//!   holds exactly as a double. There is no mode that accepts a larger
//!   integer; an exact integer past that bound must travel as a decimal
//!   string instead. This bound is on the Rust value the encoder receives,
//!   not on JSON text: a JSON number read by [`read`] is the double its text
//!   denotes, and is encoded as that double. An `f32` is widened to the `f64`
//!   with the same value, so `0.1_f32` encodes as `0.10000000149011612`;
//!   encode an `f64` when the decimal spelling is what is meant.
//! * Strings are escaped as §3.2.2.2 requires, with no Unicode normalization.
//! * Reaching a limit refuses the encoding with [`Error::Limit`], naming the
//!   limit kind and bound; the encoder never returns truncated output as
//!   success.
//!
//! # Streaming and memory
//!
//! Arrays, strings and scalars stream straight to the sink. An object's
//! members must be sorted, so while any object is open its canonical bytes
//! are buffered; a top-level object therefore reaches the sink only when it
//! is complete. Buffered bytes are canonical output and count against
//! [`Limits::max_bytes`]; on top of that come 8 bytes of offsets per buffered
//! member, small records per open and nested object, and `Vec` growth slack
//! (see [`Writer`]). `tests/memory.rs` measures the peak heap per shape: under
//! 4x the canonical length for flat objects and objects of objects, and under
//! 32x for a deep chain of objects, the worst shape per byte. [`read`] peaks
//! under 64x its input length.
//!
//! ```
//! use quire_canonical::{to_vec, FixedShape, Limits};
//!
//! #[derive(serde::Serialize, FixedShape)]
//! struct Example { b: f64, a: &'static str }
//!
//! let bytes = to_vec(&Example { b: 1e21, a: "ö" }, Limits::new(1024))?;
//! assert_eq!(bytes, "{\"a\":\"ö\",\"b\":1e+21}".as_bytes());
//! # Ok::<(), quire_canonical::Error>(())
//! ```

#![no_std]
#![warn(missing_docs)]
#![forbid(unsafe_code)]

extern crate alloc;
#[cfg(any(feature = "std", test))]
extern crate std;

mod encoder;
mod error;
mod escape;
mod identity;
mod number;
mod order;
mod read;
mod shape;
mod sink;
#[cfg(feature = "serde_json")]
mod value;
mod writer;

use alloc::vec::Vec;
use core::fmt;

use serde::{Deserialize, Serialize};

use sha2::{Digest as _, Sha256};

pub use crate::error::{Error, LimitExceeded, LimitKind, ProtocolViolation};
pub use crate::identity::{
    AuthorityDigest, AuthorityIdentity, AuthorityQualifiedSubjectReference, DigestDomain,
    IdentityError, ObjectIdentity, Revision, SemanticComparisonRefusal, SubjectKind,
};
pub use crate::read::{
    read, Document, Items, Malformed, Members, Node, NodeRef, Number, ReadError,
};
pub use crate::shape::{deepest, nest, FixedShape};
pub use crate::sink::Sink;
#[cfg(feature = "std")]
pub use crate::sink::WriteSink;
#[cfg(feature = "serde_json")]
pub use crate::value::drop_value;
pub use crate::writer::Writer;
/// `#[derive(FixedShape)]`: computes `DEPTH` from every field's `DEPTH`.
pub use quire_canonical_derive::FixedShape;

/// The explicit bounds every encoding runs under: a canonical byte ceiling,
/// and nothing else. Depth is not a limit; it costs bytes.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct Limits {
    max_bytes: u64,
}

impl Limits {
    /// Refuse an encoding whose canonical text would exceed `max_bytes` bytes.
    #[must_use]
    pub const fn new(max_bytes: u64) -> Self {
        Self { max_bytes }
    }

    /// The canonical byte ceiling. Bytes buffered for sorting are canonical
    /// output and count against it; the bookkeeping around them does not (see
    /// the crate docs).
    #[must_use]
    pub const fn max_bytes(&self) -> u64 {
        self.max_bytes
    }
}

/// A value that writes itself into a [`Writer`] as exactly one JSON value.
///
/// Every [`FixedShape`] type is one, through its serde encoding, and so are
/// [`Document`], [`NodeRef`] and, with the `serde_json` feature,
/// `serde_json::Value`. Implement it for a type whose depth follows
/// its input by pushing events from an explicit stack, never by recursion.
pub trait Encode {
    /// Push this value's events into `writer`.
    ///
    /// # Errors
    ///
    /// Whatever the writer refuses.
    fn encode_into<S: Sink + ?Sized>(&self, writer: &mut Writer<'_, S>) -> Result<(), Error>;
}

impl<T: FixedShape + ?Sized> Encode for T {
    fn encode_into<S: Sink + ?Sized>(&self, writer: &mut Writer<'_, S>) -> Result<(), Error> {
        writer.serialize(self)
    }
}

/// A SHA-256 digest of canonical bytes.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
pub struct Sha256Digest([u8; 32]);

impl Sha256Digest {
    /// The 32 digest bytes.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

/// Lowercase hex, 64 characters.
impl fmt::Display for Sha256Digest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0
            .iter()
            .try_for_each(|byte| write!(formatter, "{byte:02x}"))
    }
}

/// Encode `value` as RFC 8785 canonical text into `sink`, returning the number
/// of bytes written.
///
/// # Errors
///
/// [`Error::Limit`] when a [`Limits`] bound is reached; the other [`Error`]
/// variants when `value` has no RFC 8785 encoding or the sink fails. On any
/// error the sink may hold a prefix of the canonical text, which is not
/// canonical output and must be discarded.
pub fn encode<S, T>(sink: &mut S, value: &T, limits: Limits) -> Result<u64, Error>
where
    S: Sink + ?Sized,
    T: Encode + ?Sized,
{
    let mut writer = Writer::new(sink, limits);
    value.encode_into(&mut writer)?;
    writer.finish()
}

/// The RFC 8785 canonical bytes of `value`.
///
/// # Errors
///
/// As [`encode`]. No bytes are returned on error.
pub fn to_vec<T: Encode + ?Sized>(value: &T, limits: Limits) -> Result<Vec<u8>, Error> {
    let mut bytes = Vec::new();
    encode(&mut bytes, value, limits)?;
    Ok(bytes)
}

/// The SHA-256 digest of the RFC 8785 canonical bytes of `value`, hashed as
/// they are encoded.
///
/// # Errors
///
/// As [`encode`]. No digest is returned on error.
pub fn sha256<T: Encode + ?Sized>(value: &T, limits: Limits) -> Result<Sha256Digest, Error> {
    let mut hasher = Sha256::new();
    encode(&mut hasher, value, limits)?;
    Ok(Sha256Digest(hasher.finalize().into()))
}

/// The SHA-256 digest of a digest-domain label followed by the RFC 8785
/// canonical bytes of `value`.
///
/// The hash input is the label's byte length as a big-endian `u64`, then the
/// label, then the canonical bytes. The length prefix makes the split
/// between label and text unambiguous: `("tag", 12)` and `("tag1", 2)` hash
/// different inputs. The label is not canonical text and does not count
/// against [`Limits::max_bytes`].
///
/// # Errors
///
/// As [`encode`]. No digest is returned on error.
pub fn sha256_with_domain<T: Encode + ?Sized>(
    domain: &[u8],
    value: &T,
    limits: Limits,
) -> Result<Sha256Digest, Error> {
    let length = u64::try_from(domain.len()).map_err(|_| Error::Internal {
        invariant: "a slice length fits u64",
    })?;
    let mut hasher = Sha256::new();
    hasher.update(length.to_be_bytes());
    hasher.update(domain);
    encode(&mut hasher, value, limits)?;
    Ok(Sha256Digest(hasher.finalize().into()))
}
