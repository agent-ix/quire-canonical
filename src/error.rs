// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Agent-IX
//! The refusals the encoder returns.
//!
//! Every variant is a refusal of the whole encoding. The encoder never
//! returns a shortened or partial result as success: a digest is only
//! produced when the complete canonical text was hashed, and a byte vector is
//! only returned when it holds the complete canonical text.

use std::fmt;

/// Which configured limit an encoding reached.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum LimitKind {
    /// The number of canonical output bytes. Member buffers held for sorting
    /// are part of the output, so this also bounds them.
    CanonicalBytes,
    /// The number of simultaneously open arrays and objects.
    NestingDepth,
}

impl fmt::Display for LimitKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::CanonicalBytes => "canonical bytes",
            Self::NestingDepth => "nesting depth",
        })
    }
}

/// A configured limit was reached. The encoding was refused, not truncated.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct LimitExceeded {
    /// The limit that was reached.
    pub kind: LimitKind,
    /// The configured bound for that limit.
    pub bound: u64,
    /// The amount the encoding needed when it was refused: the output length
    /// including the bytes being written, or the depth being opened. Always
    /// greater than `bound`.
    pub required: u64,
}

impl fmt::Display for LimitExceeded {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{} limit exceeded: required {}, bound {}",
            self.kind, self.required, self.bound
        )
    }
}

impl std::error::Error for LimitExceeded {}

/// Why a value could not be encoded.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// A configured limit was reached.
    #[error(transparent)]
    Limit(#[from] LimitExceeded),
    /// A NaN or infinite float. RFC 8785 §3.2.2.3 has no representation for
    /// either.
    #[error("non-finite number {0} has no RFC 8785 representation")]
    NonFiniteNumber(f64),
    /// An integer with no exact IEEE 754 double. RFC 8785 numbers are doubles,
    /// and rounding the integer would change the value being identified.
    #[error("integer {0} is not exactly representable as an IEEE 754 double")]
    InexactInteger(i128),
    /// An unsigned integer above `i128::MAX` with no exact IEEE 754 double.
    #[error("integer {0} is not exactly representable as an IEEE 754 double")]
    InexactUnsignedInteger(u128),
    /// An object member name that is not a string.
    #[error("object member name must be a string, found {found}")]
    NonStringMemberName {
        /// The serde data-model kind that was offered as a name.
        found: &'static str,
    },
    /// Two members of one object have the same name.
    #[error("duplicate object member name {name:?}")]
    DuplicateMemberName {
        /// The repeated name.
        name: String,
    },
    /// A `serde_json` private-token struct, emitted when `serde_json`'s
    /// `arbitrary_precision` or `raw_value` feature is on. Its fields do not
    /// describe the JSON value it stands for, so encoding them would produce
    /// the wrong canonical text.
    #[error(
        "serde_json private token {0} is not supported; disable arbitrary_precision/raw_value"
    )]
    SerdeJsonPrivateToken(&'static str),
    /// A heap reservation for canonical bytes or member bookkeeping failed.
    #[error("allocation of {requested} bytes for canonical output failed")]
    Allocation {
        /// The size of the reservation that failed.
        requested: usize,
    },
    /// The byte sink refused a write.
    #[error("canonical output sink failed: {0}")]
    Sink(#[from] std::io::Error),
    /// The value's own `Serialize` implementation reported an error.
    #[error("{0}")]
    Serialize(String),
}

impl serde::ser::Error for Error {
    fn custom<T: fmt::Display>(message: T) -> Self {
        Self::Serialize(message.to_string())
    }
}
