// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Agent-IX
//! One streaming RFC 8785 (JSON Canonicalization Scheme) encoder that hashes
//! as it encodes.
//!
//! Any [`serde::Serialize`] value is encoded to its RFC 8785 canonical text
//! and fed, in order, to a [`Sink`]: a SHA-256 hasher, a byte vector, an
//! [`std::io::Write`], or a pair of them. There is no intermediate `String` of
//! the whole text and no `serde_json::Value`.
//!
//! * Member names are ordered by UTF-16 code unit (RFC 8785 §3.2.3) by the
//!   encoder itself, so the bytes do not depend on how a map type is backed,
//!   nor on `serde_json`'s `preserve_order` feature.
//! * Numbers use ECMAScript `Number::toString` (§3.2.2.3). An integer is
//!   encoded by its IEEE 754 double value and refused if no double equals it.
//!   An `f32` is widened to the `f64` with the same value, so `0.1_f32`
//!   encodes as `0.10000000149011612` (serde_json prints `0.1`); encode an
//!   `f64` when the decimal spelling is what is meant.
//! * Strings are escaped as §3.2.2.2 requires, with no Unicode normalization.
//! * Every encoding runs under explicit [`Limits`]. Reaching one refuses the
//!   encoding with [`Error::Limit`], naming the limit kind and bound; the
//!   encoder never returns truncated output as success.
//!
//! # Streaming and memory
//!
//! Arrays, strings and scalars stream straight to the sink. An object's
//! members must be sorted, so each open object buffers its members' canonical
//! bytes until it closes; a top-level object therefore reaches the sink only
//! when it is complete. Buffered bytes are canonical output and count against
//! [`Limits::max_bytes`]; on top of that come 8 bytes of offsets per buffered
//! member, `Vec` growth slack, and a transient copy while a closing object
//! moves into its parent. `tests/memory.rs` holds the measured peak heap for
//! a flat object of small members under 4x the canonical length.
//!
//! ```
//! use quire_canonical::{to_vec, Limits};
//!
//! #[derive(serde::Serialize)]
//! struct Example { b: f64, a: &'static str }
//!
//! let limits = Limits::new(1024, 16).expect("depth within MAX_DEPTH");
//! let bytes = to_vec(&Example { b: 1e21, a: "ö" }, limits)?;
//! assert_eq!(bytes, "{\"a\":\"ö\",\"b\":1e+21}".as_bytes());
//! # Ok::<(), quire_canonical::Error>(())
//! ```

#![warn(missing_docs)]
#![forbid(unsafe_code)]

mod encoder;
mod error;
mod escape;
mod number;
mod order;
mod sink;

use std::fmt;

use serde::Serialize;
use sha2::{Digest as _, Sha256};

pub use crate::error::{DepthAboveMaximum, Error, LimitExceeded, LimitKind, ProtocolViolation};
pub use crate::sink::{Sink, WriteSink};

/// The explicit bounds every encoding runs under.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct Limits {
    max_bytes: u64,
    max_depth: u32,
}

impl Limits {
    /// The deepest nesting a [`Limits`] may allow.
    ///
    /// serde drives serialization by recursion, one set of native stack
    /// frames per nesting level, so the depth limit is also the stack budget.
    /// `tests/limits.rs` encodes this depth of nested objects on a 1 MiB
    /// thread stack in a debug build; the default thread stack is 2 MiB.
    pub const MAX_DEPTH: u32 = 512;

    /// Refuse an encoding whose canonical text would exceed `max_bytes` bytes,
    /// or that opens more than `max_depth` nested arrays and objects.
    ///
    /// # Errors
    ///
    /// [`DepthAboveMaximum`] when `max_depth` exceeds [`Limits::MAX_DEPTH`].
    /// The depth is refused rather than clamped, so the limit in force is
    /// always the one the caller wrote.
    pub const fn new(max_bytes: u64, max_depth: u32) -> Result<Self, DepthAboveMaximum> {
        if max_depth > Self::MAX_DEPTH {
            return Err(DepthAboveMaximum {
                requested: max_depth,
                maximum: Self::MAX_DEPTH,
            });
        }
        Ok(Self {
            max_bytes,
            max_depth,
        })
    }

    /// The canonical byte ceiling. Bytes buffered for sorting are canonical
    /// output and count against it; the bookkeeping around them does not (see
    /// the crate docs).
    #[must_use]
    pub const fn max_bytes(&self) -> u64 {
        self.max_bytes
    }

    /// The nesting-depth ceiling.
    #[must_use]
    pub const fn max_depth(&self) -> u32 {
        self.max_depth
    }
}

/// A SHA-256 digest of canonical bytes.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
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
    T: Serialize + ?Sized,
{
    let mut encoder = encoder::Encoder::new(sink, limits);
    value.serialize(&mut encoder)?;
    Ok(encoder.produced())
}

/// The RFC 8785 canonical bytes of `value`.
///
/// # Errors
///
/// As [`encode`]. No bytes are returned on error.
pub fn to_vec<T: Serialize + ?Sized>(value: &T, limits: Limits) -> Result<Vec<u8>, Error> {
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
pub fn sha256<T: Serialize + ?Sized>(value: &T, limits: Limits) -> Result<Sha256Digest, Error> {
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
pub fn sha256_with_domain<T: Serialize + ?Sized>(
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
