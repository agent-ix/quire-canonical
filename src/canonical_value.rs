// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Agent-IX
//! An owned proof that bytes are one complete RFC 8785 value.

use alloc::vec::Vec;

use crate::{read, to_vec, Error, Limits, ReadError};

/// A complete value whose bytes have been validated as RFC 8785 canonical JSON.
///
/// Construct values with [`CanonicalValue::new`]. Its private bytes can then
/// be embedded by a [`Writer`](crate::Writer) without another encoding pass.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CanonicalValue {
    bytes: Vec<u8>,
}

/// Why bytes could not be validated as a canonical value.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum CanonicalValueError {
    /// The bytes were not exactly one valid JSON value.
    #[error(transparent)]
    Read(#[from] ReadError),
    /// Encoding the parsed value failed.
    #[error(transparent)]
    Encode(#[from] Error),
    /// The input parsed, but was not already in canonical form.
    #[error("input is valid JSON but is not RFC 8785 canonical text")]
    NotCanonical,
}

impl CanonicalValue {
    /// Validate `bytes` as one complete canonical JSON value.
    ///
    /// The input limit applies before parsing. Construction parses the input,
    /// encodes the parsed value canonically once, and retains those bytes only
    /// when they exactly match the input.
    ///
    /// # Errors
    ///
    /// [`CanonicalValueError::Read`] for malformed, incomplete, or oversized
    /// input; [`CanonicalValueError::NotCanonical`] for valid JSON with
    /// noncanonical spelling; or [`CanonicalValueError::Encode`] if canonical
    /// encoding fails.
    pub fn new(bytes: &[u8], max_input_bytes: u64) -> Result<Self, CanonicalValueError> {
        let document = read(bytes, max_input_bytes)?;
        let canonical = to_vec(&document, Limits::new(u64::MAX))?;
        if canonical != bytes {
            return Err(CanonicalValueError::NotCanonical);
        }
        Ok(Self { bytes: canonical })
    }

    /// The validated canonical bytes.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes
    }
}
