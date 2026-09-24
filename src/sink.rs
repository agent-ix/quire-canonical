// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Agent-IX
//! Where finished canonical bytes go.
//!
//! The encoder hands a [`Sink`] only bytes whose position in the canonical
//! text is final. A digest sink therefore hashes the canonical text in one
//! pass, with no copy of the whole text held anywhere.

use std::io;

use sha2::{Digest as _, Sha256};

use crate::Error;

/// A destination for canonical bytes, fed in order.
pub trait Sink {
    /// Append `bytes` to the destination.
    ///
    /// # Errors
    ///
    /// Whatever the destination refuses with; the encoding is then refused.
    fn write_bytes(&mut self, bytes: &[u8]) -> Result<(), Error>;
}

impl<S: Sink + ?Sized> Sink for &mut S {
    fn write_bytes(&mut self, bytes: &[u8]) -> Result<(), Error> {
        (**self).write_bytes(bytes)
    }
}

/// Hash the canonical bytes. Seed the hasher with a domain label first to take
/// a domain-separated digest.
impl Sink for Sha256 {
    fn write_bytes(&mut self, bytes: &[u8]) -> Result<(), Error> {
        self.update(bytes);
        Ok(())
    }
}

/// Collect the canonical bytes. Growth uses `try_reserve`, so an allocation
/// failure is a refusal rather than an abort.
impl Sink for Vec<u8> {
    fn write_bytes(&mut self, bytes: &[u8]) -> Result<(), Error> {
        try_extend(self, bytes)
    }
}

/// Feed both sinks, first `.0` then `.1`, e.g. to collect and hash at once.
impl<A: Sink, B: Sink> Sink for (A, B) {
    fn write_bytes(&mut self, bytes: &[u8]) -> Result<(), Error> {
        self.0.write_bytes(bytes)?;
        self.1.write_bytes(bytes)
    }
}

/// Adapts any [`io::Write`] into a [`Sink`].
///
/// A refused encoding may already have written a prefix of the canonical text
/// to the writer. That prefix is not canonical output; discard it on `Err`.
#[derive(Debug)]
pub struct WriteSink<W>(pub W);

impl<W: io::Write> Sink for WriteSink<W> {
    fn write_bytes(&mut self, bytes: &[u8]) -> Result<(), Error> {
        self.0.write_all(bytes).map_err(Error::Sink)
    }
}

/// Append `bytes` to `buffer`, refusing rather than aborting when the
/// reservation fails.
pub(crate) fn try_extend(buffer: &mut Vec<u8>, bytes: &[u8]) -> Result<(), Error> {
    buffer
        .try_reserve(bytes.len())
        .map_err(|_| Error::Allocation {
            requested: bytes.len(),
        })?;
    buffer.extend_from_slice(bytes);
    Ok(())
}
