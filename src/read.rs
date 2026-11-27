// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Agent-IX
//! The shared reader of untrusted JSON, its arena tree, and the tree's
//! canonical encoder.
//!
//! [`read`] parses RFC 8259 JSON text into a [`Document`] with one loop over
//! the input and an explicit heap stack of open containers; nothing recurses,
//! so a document of any depth reads on any thread stack. The tree is an
//! arena: compact, width-adaptive vectors of ranges, tags and child ids, plus decoded text. Its `Clone`,
//! `Debug`, `PartialEq` and `Drop` therefore walk flat vectors and never
//! recurse either.
//!
//! # Bounds and working-byte accounting
//!
//! There is no depth limit and no fixed size cap. Two independent provisions
//! bound a read, both the caller's:
//!
//! * the input byte limit, checked first and before any parsing, so an input
//!   over it is refused as [`LimitKind::InputBytes`] whatever it contains;
//! * the working-byte provision of [`read_with_reservation`] (a caller's
//!   [`Reservation`], so one meter can also sit behind the canonical writer)
//!   or [`read_bounded`] (a cumulative byte count).
//!
//! The reader calls [`Reservation::reserve`] once before each growth of one
//! of its lists: ranges, tags, child ids and object members, the decoded
//! text, the open-container stack, the children of open containers, the
//! duplicate-name sort buffer, and the pointer and lexeme a
//! [`ReadError::NumberOutOfRange`] or [`ReadError::DuplicateName`] carries.
//! The unit is the same as the
//! writer's: bytes of the full new capacity, element size times new
//! capacity, charged at every growth and never credited back. The reader
//! grows each list to exactly the capacity it computed (doubling, at least
//! four elements), so the bytes asked are the bytes requested from the
//! allocator and no length-based estimate is involved.
//!
//! [`read_exact_with_reservation`] asks for no growth history: one parser,
//! generic over what it builds, reads the input once to count each list and
//! once to fill it, and each list of the document is reserved once at its
//! exact final size. Its doc gives the bytes asked as a formula of the
//! document, and the outcome order that differs under a tight budget.
//!
//! Retention: the sum of the bytes asked bounds the read's peak heap, since a
//! growth that copies holds the old and the new block at once. When the read
//! returns, the open-container stack, the scratch lists and the sort buffer
//! are dropped, and the [`Document`] keeps the rest,
//! [`Document::retained_bytes`]. A refusal that carries a pointer or lexeme
//! ([`ReadError::NumberOutOfRange`], [`ReadError::DuplicateName`]) keeps those
//! charged strings for as long as the error lives; they are not part of the
//! document. A meter that does not refund therefore
//! already holds the document's charge and more; a caller whose meter
//! refunds can release down to `retained_bytes` when the read returns, and
//! the rest when it drops the document. Decoding or copying the document
//! afterwards is the caller's own growth and is charged by the caller to the
//! same meter.
//!
//! A refusal ([`ReadError::ReservationDenied`], or [`ReadError::Limit`] with
//! [`LimitKind::WorkingBytes`] from `read_bounded`) comes before the denied
//! growth, drops everything the reader holds and returns no document.
//! Growth uses `try_reserve_exact`, so exhausting memory is a different
//! refusal, [`ReadError::Allocation`], not an abort.
//!
//! # What it accepts
//!
//! Exactly one JSON value, surrounded by optional JSON whitespace, as UTF-8.
//! It refuses, with the byte offset where the fault starts (a number with no
//! finite double also carries its JSON pointer and source text):
//!
//! * text that is not UTF-8, and a byte order mark;
//! * a lone surrogate escape such as `"\ud800"`, which has no UTF-8 form;
//! * a number with no finite IEEE 754 double, such as `1e400`;
//! * two members of one object with the same name (RFC 8785 requires I-JSON);
//! * every other departure from the RFC 8259 grammar.
//!
//! A number keeps its source text after validating that it has a finite double.
//! RFC 8785 encodes that double, while a typed reader may need an integer
//! exactly as written; the same validated lexeme supplies the double when a
//! caller exposes or encodes the number.

use alloc::string::String;
use alloc::vec::Vec;
use core::fmt;

use crate::reservation::{Provision, Refusal};
mod extract;

pub use extract::ExtractError;

use crate::{
    Encode, Error, LimitExceeded, LimitKind, Reservation, ReservationDenied, Sink, Writer,
};

/// Why [`read`] refused its input.
#[derive(Clone, Debug, Eq, Hash, PartialEq, thiserror::Error)]
#[non_exhaustive]
pub enum ReadError {
    /// The input is longer than the caller's input byte limit
    /// ([`LimitKind::InputBytes`]). Distinct from every malformed-input
    /// refusal: the input was not parsed.
    #[error(transparent)]
    Limit(#[from] LimitExceeded),
    /// The input is not one well-formed JSON value.
    #[error("malformed JSON at byte {offset}: {kind}")]
    Malformed {
        /// The byte offset into the input where the fault starts.
        offset: usize,
        /// What is wrong there.
        kind: Malformed,
    },
    /// A number whose value has no finite IEEE 754 double, such as `1e400`.
    ///
    /// The number is identified by where it sits and by its source text, so a
    /// caller can classify it: a whole value beyond +-2^53, exponent forms
    /// included, is an inexact integer; any other is an inexact number.
    #[error("number {lexeme} at {pointer:?} (byte {offset}) has no finite IEEE 754 double")]
    NumberOutOfRange {
        /// The byte offset of the number's first byte.
        offset: usize,
        /// The number's location as an RFC 6901 JSON pointer (`/n`, `/0`,
        /// `~0` for `~` and `~1` for `/` in member names); empty for a
        /// top-level number.
        pointer: String,
        /// The number's exact source text, such as `-1e400`.
        lexeme: String,
    },
    /// Two members of one object have the same name (RFC 8785 requires
    /// I-JSON). Identified like [`ReadError::NumberOutOfRange`]: by where the
    /// repeat sits.
    #[error("duplicate member name at {pointer:?} (byte {offset})")]
    DuplicateName {
        /// The byte offset of the later member's name.
        offset: usize,
        /// The later member's location as an RFC 6901 JSON pointer.
        pointer: String,
    },
    /// A heap reservation for the tree failed.
    #[error("allocation of {requested} bytes for the JSON tree failed")]
    Allocation {
        /// The size of the reservation that failed.
        requested: usize,
    },
    /// The caller's [`Reservation`] refused a heap growth the reader needed
    /// ([`read_with_reservation`]). Nothing was allocated for it and no
    /// document is returned. Distinct from [`ReadError::Allocation`], which
    /// is the allocator running out.
    #[error("heap reservation of {requested} bytes for the JSON tree was denied")]
    ReservationDenied {
        /// The bytes of growth the reservation was asked for.
        requested: usize,
    },
}

impl From<Refusal> for ReadError {
    fn from(refusal: Refusal) -> Self {
        match refusal {
            Refusal::Denied { requested } => Self::ReservationDenied { requested },
            Refusal::Allocation { requested } => Self::Allocation { requested },
        }
    }
}

/// What is wrong with malformed JSON input.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
#[non_exhaustive]
pub enum Malformed {
    /// The input is not UTF-8.
    InvalidUtf8,
    /// The input ends inside a value, or holds no value.
    UnexpectedEnd,
    /// A character the grammar does not allow at this position.
    UnexpectedCharacter,
    /// An unescaped control character (below U+0020) inside a string.
    ControlCharacter,
    /// A backslash escape that RFC 8259 does not define.
    InvalidEscape,
    /// A `\u` escape of a UTF-16 surrogate that is not half of a pair.
    LoneSurrogate,
    /// A number that does not follow the RFC 8259 number grammar.
    InvalidNumber,
    /// Content after the one top-level value.
    TrailingContent,
}

impl fmt::Display for Malformed {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::InvalidUtf8 => "input is not UTF-8",
            Self::UnexpectedEnd => "unexpected end of input",
            Self::UnexpectedCharacter => "unexpected character",
            Self::ControlCharacter => "unescaped control character in string",
            Self::InvalidEscape => "invalid escape",
            Self::LoneSurrogate => "lone surrogate escape",
            Self::InvalidNumber => "invalid number",
            Self::TrailingContent => "content after the JSON value",
        })
    }
}

/// A half-open range into one of a [`Document`]'s flat vectors.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Range {
    start: usize,
    end: usize,
}

/// A temporary node value assembled from the document's paired arenas.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Slot {
    range: Range,
    tag: SlotTag,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
enum SlotTag {
    Null,
    False,
    True,
    Number,
    String,
    Array,
    Object,
}

/// The narrowest representation that can hold every index in one document.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum IndexWidth {
    Bits32,
    Bits40,
    Native,
}

impl IndexWidth {
    fn for_bound(bound: usize) -> Self {
        if u32::try_from(bound).is_ok() {
            Self::Bits32
        } else if u64::try_from(bound).is_ok_and(|value| value <= Packed40::MAX) {
            Self::Bits40
        } else {
            Self::Native
        }
    }

    fn for_dimensions(dimensions: &[usize]) -> Self {
        Self::for_bound(dimensions.iter().copied().max().unwrap_or(0))
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Range32 {
    start: u32,
    end: u32,
}

const _: () = assert!(core::mem::size_of::<Range32>() == 8);

impl Range32 {
    fn encode(range: Range) -> Option<Self> {
        Some(Self {
            start: u32::try_from(range.start).ok()?,
            end: u32::try_from(range.end).ok()?,
        })
    }

    fn decode(self) -> Option<Range> {
        Some(Range {
            start: usize::try_from(self.start).ok()?,
            end: usize::try_from(self.end).ok()?,
        })
    }
}

/// A little-endian unsigned index stored in five bytes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Packed40([u8; 5]);

const _: () = assert!(core::mem::size_of::<Packed40>() == 5);

impl Packed40 {
    const MAX: u64 = (1_u64 << 40) - 1;

    fn encode(value: usize) -> Option<Self> {
        let value = u64::try_from(value).ok()?;
        if value > Self::MAX {
            return None;
        }
        let mut bytes = [0_u8; 5];
        for (index, byte) in bytes.iter_mut().enumerate() {
            let shift = u32::try_from(index.checked_mul(8)?).ok()?;
            *byte = u8::try_from((value >> shift) & 0xff).ok()?;
        }
        Some(Self(bytes))
    }

    fn decode(self) -> Option<usize> {
        let mut value = 0_u64;
        for (index, byte) in self.0.iter().enumerate() {
            let shift = u32::try_from(index.checked_mul(8)?).ok()?;
            value |= u64::from(*byte) << shift;
        }
        usize::try_from(value).ok()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Range40 {
    start: Packed40,
    end: Packed40,
}

const _: () = assert!(core::mem::size_of::<Range40>() == 10);

impl Range40 {
    fn encode(range: Range) -> Option<Self> {
        Some(Self {
            start: Packed40::encode(range.start)?,
            end: Packed40::encode(range.end)?,
        })
    }

    fn decode(self) -> Option<Range> {
        Some(Range {
            start: self.start.decode()?,
            end: self.end.decode()?,
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Member32 {
    name: Range32,
    id: u32,
}

const _: () = assert!(core::mem::size_of::<Member32>() == 12);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Member40 {
    name: Range40,
    id: Packed40,
}

const _: () = assert!(core::mem::size_of::<Member40>() == 15);

#[derive(Clone, Debug)]
enum RangeVec {
    Bits32(Vec<Range32>),
    Bits40(Vec<Range40>),
    Native(Vec<Range>),
}

impl RangeVec {
    fn new(width: IndexWidth) -> Self {
        match width {
            IndexWidth::Bits32 => Self::Bits32(Vec::new()),
            IndexWidth::Bits40 => Self::Bits40(Vec::new()),
            IndexWidth::Native => Self::Native(Vec::new()),
        }
    }

    fn len(&self) -> usize {
        match self {
            Self::Bits32(values) => values.len(),
            Self::Bits40(values) => values.len(),
            Self::Native(values) => values.len(),
        }
    }

    fn retained_bytes(&self) -> usize {
        match self {
            Self::Bits32(values) => bytes(values),
            Self::Bits40(values) => bytes(values),
            Self::Native(values) => bytes(values),
        }
    }

    fn get(&self, index: usize) -> Option<Range> {
        match self {
            Self::Bits32(values) => values.get(index)?.decode(),
            Self::Bits40(values) => values.get(index)?.decode(),
            Self::Native(values) => values.get(index).copied(),
        }
    }

    fn reserve_exact(
        &mut self,
        provision: &mut Provision<'_>,
        length: usize,
    ) -> Result<(), ReadError> {
        match self {
            Self::Bits32(values) => reserve_exact(provision, values, length),
            Self::Bits40(values) => reserve_exact(provision, values, length),
            Self::Native(values) => reserve_exact(provision, values, length),
        }
    }

    fn grow_push(&mut self, provision: &mut Provision<'_>, range: Range) -> Result<(), ReadError> {
        match self {
            Self::Bits32(values) => {
                let range = Range32::encode(range).ok_or_else(index_allocation_error)?;
                provision.grow(values, 1).map_err(ReadError::from)?;
                values.push(range);
            }
            Self::Bits40(values) => {
                let range = Range40::encode(range).ok_or_else(index_allocation_error)?;
                provision.grow(values, 1).map_err(ReadError::from)?;
                values.push(range);
            }
            Self::Native(values) => {
                provision.grow(values, 1).map_err(ReadError::from)?;
                values.push(range);
            }
        }
        Ok(())
    }

    fn push_reserved(&mut self, range: Range) -> Option<()> {
        match self {
            Self::Bits32(values) => values.push(Range32::encode(range)?),
            Self::Bits40(values) => values.push(Range40::encode(range)?),
            Self::Native(values) => values.push(range),
        }
        Some(())
    }

    fn logical_eq(&self, other: &Self) -> bool {
        self.len() == other.len()
            && (0..self.len()).all(|index| self.get(index) == other.get(index))
    }
}

#[derive(Clone, Debug)]
enum IndexVec {
    Bits32(Vec<u32>),
    Bits40(Vec<Packed40>),
    Native(Vec<usize>),
}

impl IndexVec {
    fn new(width: IndexWidth) -> Self {
        match width {
            IndexWidth::Bits32 => Self::Bits32(Vec::new()),
            IndexWidth::Bits40 => Self::Bits40(Vec::new()),
            IndexWidth::Native => Self::Native(Vec::new()),
        }
    }

    fn len(&self) -> usize {
        match self {
            Self::Bits32(values) => values.len(),
            Self::Bits40(values) => values.len(),
            Self::Native(values) => values.len(),
        }
    }

    fn retained_bytes(&self) -> usize {
        match self {
            Self::Bits32(values) => bytes(values),
            Self::Bits40(values) => bytes(values),
            Self::Native(values) => bytes(values),
        }
    }

    fn get(&self, index: usize) -> Option<usize> {
        match self {
            Self::Bits32(values) => usize::try_from(*values.get(index)?).ok(),
            Self::Bits40(values) => values.get(index)?.decode(),
            Self::Native(values) => values.get(index).copied(),
        }
    }

    fn iter_range(&self, range: Range) -> Option<IndexIter<'_>> {
        Some(match self {
            Self::Bits32(values) => IndexIter::Bits32(values.get(range.start..range.end)?.iter()),
            Self::Bits40(values) => IndexIter::Bits40(values.get(range.start..range.end)?.iter()),
            Self::Native(values) => IndexIter::Native(values.get(range.start..range.end)?.iter()),
        })
    }

    fn reserve_exact(
        &mut self,
        provision: &mut Provision<'_>,
        length: usize,
    ) -> Result<(), ReadError> {
        match self {
            Self::Bits32(values) => reserve_exact(provision, values, length),
            Self::Bits40(values) => reserve_exact(provision, values, length),
            Self::Native(values) => reserve_exact(provision, values, length),
        }
    }

    fn grow_for(
        &mut self,
        provision: &mut Provision<'_>,
        additional: usize,
    ) -> Result<(), ReadError> {
        match self {
            Self::Bits32(values) => provision.grow(values, additional),
            Self::Bits40(values) => provision.grow(values, additional),
            Self::Native(values) => provision.grow(values, additional),
        }
        .map_err(ReadError::from)
    }

    fn grow_push(&mut self, provision: &mut Provision<'_>, id: usize) -> Result<(), ReadError> {
        match self {
            Self::Bits32(values) => {
                let id = u32::try_from(id).map_err(|_| index_allocation_error())?;
                provision.grow(values, 1).map_err(ReadError::from)?;
                values.push(id);
            }
            Self::Bits40(values) => {
                let id = Packed40::encode(id).ok_or_else(index_allocation_error)?;
                provision.grow(values, 1).map_err(ReadError::from)?;
                values.push(id);
            }
            Self::Native(values) => {
                provision.grow(values, 1).map_err(ReadError::from)?;
                values.push(id);
            }
        }
        Ok(())
    }

    fn push_reserved(&mut self, id: usize) -> Option<()> {
        match self {
            Self::Bits32(values) => {
                values.push(u32::try_from(id).ok()?);
            }
            Self::Bits40(values) => values.push(Packed40::encode(id)?),
            Self::Native(values) => values.push(id),
        }
        Some(())
    }

    fn logical_eq(&self, other: &Self) -> bool {
        self.len() == other.len()
            && (0..self.len()).all(|index| self.get(index) == other.get(index))
    }
}

#[derive(Clone, Debug)]
enum MemberVec {
    Bits32(Vec<Member32>),
    Bits40(Vec<Member40>),
    Native(Vec<(Range, usize)>),
}

impl MemberVec {
    fn new(width: IndexWidth) -> Self {
        match width {
            IndexWidth::Bits32 => Self::Bits32(Vec::new()),
            IndexWidth::Bits40 => Self::Bits40(Vec::new()),
            IndexWidth::Native => Self::Native(Vec::new()),
        }
    }

    fn len(&self) -> usize {
        match self {
            Self::Bits32(values) => values.len(),
            Self::Bits40(values) => values.len(),
            Self::Native(values) => values.len(),
        }
    }

    fn retained_bytes(&self) -> usize {
        match self {
            Self::Bits32(values) => bytes(values),
            Self::Bits40(values) => bytes(values),
            Self::Native(values) => bytes(values),
        }
    }

    fn get(&self, index: usize) -> Option<(Range, usize)> {
        Some(match self {
            Self::Bits32(values) => {
                let member = values.get(index)?;
                (member.name.decode()?, usize::try_from(member.id).ok()?)
            }
            Self::Bits40(values) => {
                let member = values.get(index)?;
                (member.name.decode()?, member.id.decode()?)
            }
            Self::Native(values) => *values.get(index)?,
        })
    }

    fn iter_range(&self, range: Range) -> Option<MemberIter<'_>> {
        Some(match self {
            Self::Bits32(values) => MemberIter::Bits32(values.get(range.start..range.end)?.iter()),
            Self::Bits40(values) => MemberIter::Bits40(values.get(range.start..range.end)?.iter()),
            Self::Native(values) => MemberIter::Native(values.get(range.start..range.end)?.iter()),
        })
    }

    fn reserve_exact(
        &mut self,
        provision: &mut Provision<'_>,
        length: usize,
    ) -> Result<(), ReadError> {
        match self {
            Self::Bits32(values) => reserve_exact(provision, values, length),
            Self::Bits40(values) => reserve_exact(provision, values, length),
            Self::Native(values) => reserve_exact(provision, values, length),
        }
    }

    fn grow_for(
        &mut self,
        provision: &mut Provision<'_>,
        additional: usize,
    ) -> Result<(), ReadError> {
        match self {
            Self::Bits32(values) => provision.grow(values, additional),
            Self::Bits40(values) => provision.grow(values, additional),
            Self::Native(values) => provision.grow(values, additional),
        }
        .map_err(ReadError::from)
    }

    fn push_reserved(&mut self, name: Range, id: usize) -> Option<()> {
        match self {
            Self::Bits32(values) => values.push(Member32 {
                name: Range32::encode(name)?,
                id: u32::try_from(id).ok()?,
            }),
            Self::Bits40(values) => values.push(Member40 {
                name: Range40::encode(name)?,
                id: Packed40::encode(id)?,
            }),
            Self::Native(values) => values.push((name, id)),
        }
        Some(())
    }

    fn logical_eq(&self, other: &Self) -> bool {
        self.len() == other.len()
            && (0..self.len()).all(|index| self.get(index) == other.get(index))
    }
}

fn bytes<T>(values: &Vec<T>) -> usize {
    values.capacity().saturating_mul(core::mem::size_of::<T>())
}

fn index_allocation_error() -> ReadError {
    ReadError::Allocation {
        requested: usize::MAX,
    }
}

#[derive(Clone, Debug)]
enum IndexIter<'a> {
    Empty(core::iter::Empty<usize>),
    Bits32(core::slice::Iter<'a, u32>),
    Bits40(core::slice::Iter<'a, Packed40>),
    Native(core::slice::Iter<'a, usize>),
}

impl Iterator for IndexIter<'_> {
    type Item = usize;

    fn next(&mut self) -> Option<Self::Item> {
        match self {
            Self::Empty(values) => values.next(),
            Self::Bits32(values) => values.next().and_then(|value| usize::try_from(*value).ok()),
            Self::Bits40(values) => values.next().and_then(|value| value.decode()),
            Self::Native(values) => values.next().copied(),
        }
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let length = match self {
            Self::Empty(values) => values.len(),
            Self::Bits32(values) => values.len(),
            Self::Bits40(values) => values.len(),
            Self::Native(values) => values.len(),
        };
        (length, Some(length))
    }
}

impl DoubleEndedIterator for IndexIter<'_> {
    fn next_back(&mut self) -> Option<Self::Item> {
        match self {
            Self::Empty(values) => values.next_back(),
            Self::Bits32(values) => values
                .next_back()
                .and_then(|value| usize::try_from(*value).ok()),
            Self::Bits40(values) => values.next_back().and_then(|value| value.decode()),
            Self::Native(values) => values.next_back().copied(),
        }
    }
}

impl ExactSizeIterator for IndexIter<'_> {}

impl<'a> IndexIter<'a> {
    fn empty() -> Self {
        Self::Empty(core::iter::empty())
    }
}

#[derive(Clone, Debug)]
enum MemberIter<'a> {
    Empty(core::iter::Empty<(Range, usize)>),
    Bits32(core::slice::Iter<'a, Member32>),
    Bits40(core::slice::Iter<'a, Member40>),
    Native(core::slice::Iter<'a, (Range, usize)>),
}

impl Iterator for MemberIter<'_> {
    type Item = (Range, usize);

    fn next(&mut self) -> Option<Self::Item> {
        match self {
            Self::Empty(values) => values.next(),
            Self::Bits32(values) => values
                .next()
                .and_then(|member| Some((member.name.decode()?, usize::try_from(member.id).ok()?))),
            Self::Bits40(values) => values
                .next()
                .and_then(|member| Some((member.name.decode()?, member.id.decode()?))),
            Self::Native(values) => values.next().copied(),
        }
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let length = match self {
            Self::Empty(values) => values.len(),
            Self::Bits32(values) => values.len(),
            Self::Bits40(values) => values.len(),
            Self::Native(values) => values.len(),
        };
        (length, Some(length))
    }
}

impl DoubleEndedIterator for MemberIter<'_> {
    fn next_back(&mut self) -> Option<Self::Item> {
        match self {
            Self::Empty(values) => values.next_back(),
            Self::Bits32(values) => values
                .next_back()
                .and_then(|member| Some((member.name.decode()?, usize::try_from(member.id).ok()?))),
            Self::Bits40(values) => values
                .next_back()
                .and_then(|member| Some((member.name.decode()?, member.id.decode()?))),
            Self::Native(values) => values.next_back().copied(),
        }
    }
}

impl ExactSizeIterator for MemberIter<'_> {}

impl<'a> MemberIter<'a> {
    fn empty() -> Self {
        Self::Empty(core::iter::empty())
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum SlotKind {
    Null,
    Bool(bool),
    Number { text: Range, value: f64 },
    String(Range),
    Array(Range),
    Object(Range),
}

impl Slot {
    const fn null() -> Self {
        Self {
            range: Range { start: 0, end: 0 },
            tag: SlotTag::Null,
        }
    }

    const fn boolean(value: bool) -> Self {
        let tag = if value { SlotTag::True } else { SlotTag::False };
        Self {
            range: Range { start: 0, end: 0 },
            tag,
        }
    }

    const fn number(text: Range) -> Self {
        Self {
            range: text,
            tag: SlotTag::Number,
        }
    }

    const fn string(text: Range) -> Self {
        Self {
            range: text,
            tag: SlotTag::String,
        }
    }

    const fn array(children: Range) -> Self {
        Self {
            range: children,
            tag: SlotTag::Array,
        }
    }

    const fn object(members: Range) -> Self {
        Self {
            range: members,
            tag: SlotTag::Object,
        }
    }

    fn kind(self, document: &Document) -> SlotKind {
        match self.tag {
            SlotTag::Null => SlotKind::Null,
            SlotTag::False => SlotKind::Bool(false),
            SlotTag::True => SlotKind::Bool(true),
            SlotTag::Number => SlotKind::Number {
                text: self.range,
                value: validated_number_value(document.text_of(self.range)),
            },
            SlotTag::String => SlotKind::String(self.range),
            SlotTag::Array => SlotKind::Array(self.range),
            SlotTag::Object => SlotKind::Object(self.range),
        }
    }
}

fn validated_number_value(text: &str) -> f64 {
    // The parser validates this exact lexeme as a finite f64 before creating
    // its Number slot, and the document's text is immutable afterwards.
    text.parse::<f64>()
        .expect("a stored number lexeme was validated by the parser")
}

/// A JSON document read by [`read`]: an arena tree of its values.
///
/// Every node, child list and string lives in a flat vector, so cloning,
/// comparing, printing and dropping a document never recurse, whatever its
/// depth.
///
/// Equality is structural, not JSON value equality: two documents are equal
/// when they hold the same members in the same order and the same number
/// spellings. `{"a":1,"b":2}` and `{"b":2,"a":1}` differ, and so do `1` and
/// `1.0`. Two documents denote the same JSON value exactly when their RFC
/// 8785 bytes are equal.
#[derive(Clone, Debug)]
pub struct Document {
    /// Decoded strings and number literals, back to back.
    text: String,
    ranges: RangeVec,
    tags: Vec<SlotTag>,
    items: IndexVec,
    members: MemberVec,
    root: usize,
}

impl PartialEq for Document {
    fn eq(&self, other: &Self) -> bool {
        self.text == other.text
            && self.root == other.root
            && self.tags == other.tags
            && self.ranges.logical_eq(&other.ranges)
            && self.items.logical_eq(&other.items)
            && self.members.logical_eq(&other.members)
    }
}

/// The position of one value in a [`Document`]'s arena.
///
/// An id is stable for the life of the document that produced it (a document
/// is immutable) and is read back with [`Document::node`]. It is a plain
/// index: no allocation, and an id means nothing in another document. Ids
/// order by creation, children before the container that holds them.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct NodeId(usize);

/// One value inside a [`Document`].
#[derive(Clone, Copy)]
pub struct NodeRef<'d> {
    document: &'d Document,
    id: usize,
}

impl fmt::Debug for NodeRef<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_tuple("NodeRef").field(&self.id).finish()
    }
}

/// What one [`NodeRef`] holds.
#[derive(Clone, Debug)]
pub enum Node<'d> {
    /// `null`.
    Null,
    /// `true` or `false`.
    Bool(bool),
    /// A number.
    Number(Number<'d>),
    /// A string, with its escapes decoded.
    String(&'d str),
    /// An array's elements, in order.
    Array(Items<'d>),
    /// An object's members, in document order.
    Object(Members<'d>),
}

/// A JSON number: its source text and the double it denotes.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Number<'d> {
    text: &'d str,
    value: f64,
}

impl<'d> Number<'d> {
    /// The literal exactly as written in the input, e.g. `1.50` or `1E2`.
    #[must_use]
    pub fn text(&self) -> &'d str {
        self.text
    }

    /// The nearest IEEE 754 double, the value RFC 8785 encodes.
    #[must_use]
    pub fn value(&self) -> f64 {
        self.value
    }
}

/// The elements of an array.
#[derive(Clone)]
pub struct Items<'d> {
    document: &'d Document,
    ids: IndexIter<'d>,
}

impl fmt::Debug for Items<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Items")
            .field("remaining", &self.ids.len())
            .finish()
    }
}

impl<'d> Iterator for Items<'d> {
    type Item = NodeRef<'d>;

    fn next(&mut self) -> Option<NodeRef<'d>> {
        let id = self.ids.next()?;
        Some(NodeRef {
            document: self.document,
            id,
        })
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.ids.size_hint()
    }
}

impl ExactSizeIterator for Items<'_> {}

/// The members of an object, as `(name, value)` in document order.
#[derive(Clone)]
pub struct Members<'d> {
    document: &'d Document,
    pairs: MemberIter<'d>,
}

impl fmt::Debug for Members<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Members")
            .field("remaining", &self.pairs.len())
            .finish()
    }
}

impl<'d> Iterator for Members<'d> {
    type Item = (&'d str, NodeRef<'d>);

    fn next(&mut self) -> Option<(&'d str, NodeRef<'d>)> {
        let (name, id) = self.pairs.next()?;
        Some((
            self.document.text_of(name),
            NodeRef {
                document: self.document,
                id,
            },
        ))
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.pairs.size_hint()
    }
}

impl ExactSizeIterator for Members<'_> {}

impl Document {
    /// The heap bytes this document holds: the capacity of its range, tag,
    /// child, member and text lists, the unit the reader's reservation is asked in.
    #[must_use]
    pub fn retained_bytes(&self) -> usize {
        self.text
            .capacity()
            .saturating_add(self.ranges.retained_bytes())
            .saturating_add(bytes(&self.tags))
            .saturating_add(self.items.retained_bytes())
            .saturating_add(self.members.retained_bytes())
    }

    /// The value at `id`, or `None` when `id` is past this document's last
    /// value, as an id from a larger document is. An id from another document
    /// of at least this size names some value here, not the one it named
    /// there. Does not allocate.
    #[must_use]
    pub fn node(&self, id: NodeId) -> Option<NodeRef<'_>> {
        (id.0 < self.ranges.len()).then_some(NodeRef {
            document: self,
            id: id.0,
        })
    }

    /// The top-level value.
    #[must_use]
    pub fn root(&self) -> NodeRef<'_> {
        NodeRef {
            document: self,
            id: self.root,
        }
    }

    // Ids and ranges are produced only by the reader, which creates each one
    // inside its vector, and a document is immutable afterwards; so these
    // lookups cannot miss. They fall back to an empty value rather than
    // panic.
    fn text_of(&self, range: Range) -> &str {
        self.text.get(range.start..range.end).unwrap_or_default()
    }

    fn slot(&self, id: usize) -> Slot {
        self.checked_slot(id).unwrap_or_else(Slot::null)
    }

    fn checked_slot(&self, id: usize) -> Option<Slot> {
        Some(Slot {
            range: self.ranges.get(id)?,
            tag: *self.tags.get(id)?,
        })
    }

    fn item_ids(&self, range: Range) -> Option<IndexIter<'_>> {
        self.items.iter_range(range)
    }

    fn member_pairs(&self, range: Range) -> Option<MemberIter<'_>> {
        self.members.iter_range(range)
    }
}

impl<'d> NodeRef<'d> {
    /// This value's position in its document, for [`Document::node`].
    #[must_use]
    pub fn id(&self) -> NodeId {
        NodeId(self.id)
    }

    /// What this value holds.
    #[must_use]
    pub fn node(&self) -> Node<'d> {
        let document = self.document;
        match document.slot(self.id).kind(document) {
            SlotKind::Null => Node::Null,
            SlotKind::Bool(value) => Node::Bool(value),
            SlotKind::Number { text, value } => Node::Number(Number {
                text: document.text_of(text),
                value,
            }),
            SlotKind::String(text) => Node::String(document.text_of(text)),
            SlotKind::Array(range) => Node::Array(Items {
                document,
                ids: document.item_ids(range).unwrap_or_else(IndexIter::empty),
            }),
            SlotKind::Object(range) => Node::Object(Members {
                document,
                pairs: document
                    .member_pairs(range)
                    .unwrap_or_else(MemberIter::empty),
            }),
        }
    }

    /// The value of member `name` when this is an object that has one.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<NodeRef<'d>> {
        match self.node() {
            Node::Object(mut members) => members
                .find(|(member, _)| *member == name)
                .map(|(_, value)| value),
            _ => None,
        }
    }
}

/// The whole document's RFC 8785 text.
impl Encode for Document {
    fn encode_into<S: Sink + ?Sized>(&self, writer: &mut Writer<'_, S>) -> Result<(), Error> {
        self.root().encode_into(writer)
    }
}

/// This value's RFC 8785 text, written from an explicit heap stack. The stack
/// holds at most two entries per node of the document, so its growth is
/// bounded by the document already in memory.
impl Encode for NodeRef<'_> {
    fn encode_into<S: Sink + ?Sized>(&self, writer: &mut Writer<'_, S>) -> Result<(), Error> {
        enum Task {
            Value(usize),
            Name(Range),
            EndArray,
            EndObject,
        }
        let document = self.document;
        let mut tasks = Vec::new();
        writer.reserve_list(&mut tasks, 1)?;
        tasks.push(Task::Value(self.id));
        while let Some(task) = tasks.pop() {
            match task {
                Task::Value(id) => match document.slot(id).kind(document) {
                    SlotKind::Null => writer.null()?,
                    SlotKind::Bool(value) => writer.bool(value)?,
                    SlotKind::Number { value, .. } => writer.number(value)?,
                    SlotKind::String(text) => writer.string(document.text_of(text))?,
                    SlotKind::Array(range) => {
                        writer.begin_array()?;
                        let mut ids = document.item_ids(range).unwrap_or_else(IndexIter::empty);
                        writer.reserve_list(&mut tasks, ids.len().saturating_add(1))?;
                        tasks.push(Task::EndArray);
                        tasks.extend(ids.by_ref().rev().map(Task::Value));
                    }
                    SlotKind::Object(range) => {
                        writer.begin_object()?;
                        let mut pairs = document
                            .member_pairs(range)
                            .unwrap_or_else(MemberIter::empty);
                        writer.reserve_list(
                            &mut tasks,
                            pairs.len().saturating_mul(2).saturating_add(1),
                        )?;
                        tasks.push(Task::EndObject);
                        for (name, id) in pairs.by_ref().rev() {
                            tasks.push(Task::Value(id));
                            tasks.push(Task::Name(name));
                        }
                    }
                },
                Task::Name(name) => writer.name(document.text_of(name))?,
                Task::EndArray => writer.end_array()?,
                Task::EndObject => writer.end_object()?,
            }
        }
        Ok(())
    }
}

/// Read `input` as one JSON value into a [`Document`].
///
/// # Errors
///
/// [`ReadError::Limit`] when `input` is longer than `max_input_bytes`,
/// before any of it is read; [`ReadError::Malformed`] with the byte offset of
/// the fault otherwise (see the module docs for what is refused);
/// [`ReadError::NumberOutOfRange`] with the number's JSON pointer and source
/// text when a number has no finite double; [`ReadError::Allocation`] when
/// memory runs out.
pub fn read(input: &[u8], max_input_bytes: u64) -> Result<Document, ReadError> {
    read_provisioned(input, max_input_bytes, Provision::default())
}

/// As [`read`], with the reader asking `reservation` before every growth of
/// its own heap (see the module docs for the accounting unit).
///
/// # Errors
///
/// As [`read`], and [`ReadError::ReservationDenied`] when `reservation`
/// refuses a growth. The encoded-input limit is checked first, so an input
/// over `max_input_bytes` is refused as [`ReadError::Limit`] without
/// `reservation` being asked anything.
pub fn read_with_reservation(
    input: &[u8],
    max_input_bytes: u64,
    reservation: &mut dyn Reservation,
) -> Result<Document, ReadError> {
    read_provisioned(input, max_input_bytes, Provision::new(reservation))
}

/// As [`read_with_reservation`], but every list of the [`Document`] is
/// allocated once, at its exact final size, so the reservation is asked for
/// the document itself rather than for its growth history.
///
/// The input is read twice by the one parser. Pass 1 runs the grammar with a
/// builder that only counts: decoded text bytes `T`, nodes `V`, array child
/// ids `I`, object members `M`, and the most children of open arrays `Hi`,
/// of open objects `Hm`, and of one object whose names are sorted for the
/// duplicate check `Ho`. Its only heap is the open-container stack, which
/// grows as in [`read_with_reservation`] (doubling from four entries of 16
/// bytes) to the deepest nesting `D` and is reused by pass 2. Pass 2 asks
/// `reservation` once per list, before that list's one allocation, in the
/// order text (`T` bytes), ranges, tags, child ids, members, open-array
/// children (`8·Hi`), open-object members (`32·Hm`)
/// and sort buffer (`8·Ho`), skipping an empty list; then it fills them with
/// the same grammar, and no list grows again. For an accepted input the
/// bytes asked are therefore
///
/// ```text
/// S(D) + T + R·V + V + J·I + K·M + 8·Hi + 32·Hm + 8·Ho
/// ```
///
/// where `S(D)` is the stack's growth (`16·(4 + 8 + … + c)` for the first
/// doubling `c ≥ D`, zero when `D = 0`), a function of the document alone
/// and less than [`read_with_reservation`] asks for the same input. The
/// document's [`Document::retained_bytes`] is exactly `T + R·V + V + J·I + K·M`.
/// For a 64-bit target, `(R,J,K)` is `(8,4,12)` when every index fits in 32
/// bits, `(10,5,15)` when every index fits in 40 bits, and `(16,8,24)` otherwise.
/// The chosen width is the narrowest that can represent every byte offset and
/// arena index; the 40-bit fields are explicitly packed.
///
/// The document, and every refusal with its offset, pointer and lexeme, are
/// those of [`read_with_reservation`] whenever `reservation` admits every
/// request. Under a tight budget the order of outcomes can differ, because
/// the arena is reserved before pass 2 reads anything:
///
/// * a malformed input is refused by pass 1, before any arena reservation,
///   when no object of two or more members closes before the fault (so no
///   duplicate name can come first). A malformed document is malformed
///   whatever the budget, so this refusal can now come where the growing
///   reader would have stopped on the budget;
/// * otherwise a refusal (a duplicate name, a number with no finite double,
///   or a malformed input after an object whose names are not yet checked)
///   is made by pass 2, after it reserved the lists for the input up to the
///   fault; under a budget too small for those, the outcome is
///   [`ReadError::ReservationDenied`] where the growing reader, asking less
///   by that point, could have reached the refusal.
///
/// ```
/// use quire_canonical::{read, read_exact_with_reservation, Reservation, ReservationDenied};
///
/// struct Meter(Vec<usize>);
/// impl Reservation for Meter {
///     fn reserve(&mut self, bytes: usize) -> Result<(), ReservationDenied> {
///         self.0.push(bytes);
///         Ok(())
///     }
/// }
///
/// let input = br#"{"a":[1,2],"b":"x"}"#;
/// let mut meter = Meter(Vec::new());
/// let document = read_exact_with_reservation(input, 1024, &mut meter)?;
/// assert_eq!(document, read(input, 1024)?);
/// // Stack (4 entries), then text "a12bx" (5), 5 nodes, 2 child ids,
/// // 2 members, 2 open-array children, 2 open-object members, sort buffer 2.
/// assert_eq!(meter.0, [64, 5, 5 * 8, 5, 2 * 4, 2 * 12, 2 * 8, 2 * 32, 2 * 8]);
/// assert_eq!(document.retained_bytes(), 5 + 5 * 8 + 5 + 2 * 4 + 2 * 12);
/// # Ok::<(), quire_canonical::ReadError>(())
/// ```
///
/// # Errors
///
/// As [`read_with_reservation`]: [`ReadError::Limit`] for an input over
/// `max_input_bytes`, before anything is read or asked;
/// [`ReadError::ReservationDenied`] when `reservation` refuses a request,
/// with nothing allocated for it and nothing asked after it;
/// [`ReadError::Allocation`] when the allocator refuses.
pub fn read_exact_with_reservation(
    input: &[u8],
    max_input_bytes: u64,
    reservation: &mut dyn Reservation,
) -> Result<Document, ReadError> {
    read_exact_provisioned(input, max_input_bytes, Provision::new(reservation))
}

/// As [`read`], with a working-byte provision of `max_working_bytes`: the
/// total bytes of heap the reader may grow, charged cumulatively (see the
/// module docs).
///
/// # Errors
///
/// As [`read`], and [`ReadError::Limit`] with [`LimitKind::WorkingBytes`]
/// when a growth would take the total past `max_working_bytes`; its
/// `required` is the total including that growth. No document is returned.
pub fn read_bounded(
    input: &[u8],
    max_input_bytes: u64,
    max_working_bytes: u64,
) -> Result<Document, ReadError> {
    let mut budget = WorkingBudget {
        bound: max_working_bytes,
        charged: 0,
        refused: None,
    };
    match read_with_reservation(input, max_input_bytes, &mut budget) {
        Err(ReadError::ReservationDenied { .. }) => Err(LimitExceeded {
            kind: LimitKind::WorkingBytes,
            bound: max_working_bytes,
            required: budget.refused.unwrap_or(u64::MAX),
        }
        .into()),
        other => other,
    }
}

/// A cumulative byte budget for [`read_bounded`].
struct WorkingBudget {
    bound: u64,
    charged: u64,
    /// The total the refused growth would have reached.
    refused: Option<u64>,
}

impl Reservation for WorkingBudget {
    fn reserve(&mut self, bytes: usize) -> Result<(), ReservationDenied> {
        let total = self
            .charged
            .saturating_add(u64::try_from(bytes).unwrap_or(u64::MAX));
        if total > self.bound {
            self.refused = Some(total);
            return Err(ReservationDenied);
        }
        self.charged = total;
        Ok(())
    }
}

/// Refuse an input over `max_input_bytes` before any of it is read, then
/// check that it is UTF-8.
fn admit(input: &[u8], max_input_bytes: u64) -> Result<&str, ReadError> {
    let length = u64::try_from(input.len()).unwrap_or(u64::MAX);
    if length > max_input_bytes {
        return Err(LimitExceeded {
            kind: LimitKind::InputBytes,
            bound: max_input_bytes,
            required: length,
        }
        .into());
    }
    core::str::from_utf8(input).map_err(|error| ReadError::Malformed {
        offset: error.valid_up_to(),
        kind: Malformed::InvalidUtf8,
    })
}

fn read_provisioned(
    input: &[u8],
    max_input_bytes: u64,
    provision: Provision<'_>,
) -> Result<Document, ReadError> {
    let source = admit(input, max_input_bytes)?;
    let width = IndexWidth::for_bound(source.len());
    let mut parser = Parser::new(source, provision, Vec::new(), Build::new(width));
    let root = parser.run()?;
    Ok(parser.builder.into_document(root))
}

fn read_exact_provisioned(
    input: &[u8],
    max_input_bytes: u64,
    provision: Provision<'_>,
) -> Result<Document, ReadError> {
    let source = admit(input, max_input_bytes)?;
    let width = IndexWidth::for_bound(source.len());
    // Pass 1: the grammar with the counting builder. Its only heap is the
    // open-container stack.
    let mut counting = Parser::new(source, provision, Vec::new(), Count::default());
    let counted = counting.run();
    let Parser {
        mut provision,
        mut stack,
        builder: count,
        ..
    } = counting;
    match counted {
        Ok(_) => {}
        // No object of two or more members closed before the fault, so no
        // duplicate name can precede it: this is the growing reader's first
        // refusal too.
        Err(malformed @ ReadError::Malformed { .. }) if !count.names_unchecked => {
            return Err(malformed);
        }
        // A duplicate name may come first, or the refusal needs the decoded
        // names for its pointer: pass 2 reads up to the fault and refuses
        // exactly as the growing reader does.
        Err(ReadError::Malformed { .. } | ReadError::NumberOutOfRange { .. }) => {}
        Err(other) => return Err(other),
    }
    // Pass 2: every list once at its counted size, then the same grammar
    // with the building builder. The stack keeps pass 1's capacity, which
    // already holds the deepest nesting.
    stack.clear();
    let build = Build::exact(&count, width, &mut provision)?;
    let mut parser = Parser::new(source, provision, stack, build);
    let root = parser.run()?;
    Ok(parser.builder.into_document(root))
}

/// What the parser expects next.
#[derive(Clone, Copy, Eq, PartialEq)]
enum Expect {
    /// A value.
    Value,
    /// A value or `]`, just after `[`.
    ValueOrEnd,
    /// A member name or `}`, just after `{`.
    NameOrEnd,
    /// A member name, after `,` in an object.
    Name,
    /// `,` or the innermost container's close, after a value; the end of
    /// input at the top level.
    Separator,
}

/// One open container while reading.
#[derive(Clone, Copy)]
struct OpenContainer {
    object: bool,
    /// Where this container's children start on the builder's open-children
    /// list of its kind ([`Builder::children`]).
    start: usize,
}

/// An object member read but not yet attached to its closed object. It is
/// pushed when its name is read; `id` is set when its value completes.
struct PendingMember {
    name: Range,
    /// The name's byte offset in the input.
    offset: usize,
    id: usize,
}

/// What the parser does with what it reads. The parser owns the grammar,
/// the position and the open-container stack; a builder owns every other
/// list. [`Build`] makes the [`Document`]; [`Count`] only counts what
/// [`Build`] would store, so both passes of [`read_exact_with_reservation`]
/// run the one grammar.
trait Builder {
    /// The decoded text's length so far.
    fn text_len(&self) -> usize;
    /// Append `piece` to the decoded text.
    fn append(&mut self, provision: &mut Provision<'_>, piece: &str) -> Result<(), ReadError>;
    /// Create a node; its id.
    fn node(&mut self, provision: &mut Provision<'_>, slot: Slot) -> Result<usize, ReadError>;
    /// The nodes created so far.
    fn node_count(&self) -> usize;
    /// The length of the open-children list of open objects (`object`) or
    /// open arrays.
    fn children(&self, object: bool) -> usize;
    /// A finished value `id` of the innermost open array.
    fn push_item(&mut self, provision: &mut Provision<'_>, id: usize) -> Result<(), ReadError>;
    /// A member name of the innermost open object.
    fn push_member(
        &mut self,
        provision: &mut Provision<'_>,
        member: PendingMember,
    ) -> Result<(), ReadError>;
    /// The value `id` of the innermost open object's last member; `false`
    /// when that object has no member.
    fn set_member_value(&mut self, id: usize) -> bool;
    /// Close `open`, popped off `stack`: refuse a duplicate name in an
    /// object, move its children to the document lists, and return its slot.
    fn close(
        &mut self,
        provision: &mut Provision<'_>,
        stack: &[OpenContainer],
        open: OpenContainer,
    ) -> Result<Slot, ReadError>;
    /// The refusal for the number `literal` at `offset`, inside `stack`.
    fn number_out_of_range(
        &mut self,
        provision: &mut Provision<'_>,
        stack: &[OpenContainer],
        offset: usize,
        literal: &str,
    ) -> ReadError;
}

/// The builder that makes the [`Document`], growing each list through the
/// provision as it fills.
struct Build {
    text: String,
    ranges: RangeVec,
    tags: Vec<SlotTag>,
    items: IndexVec,
    members: MemberVec,
    /// The children of every open array, innermost last.
    scratch_items: Vec<usize>,
    /// The members of every open object, innermost last.
    scratch_members: Vec<PendingMember>,
    /// Reused order buffer for the duplicate-name check.
    order: Vec<usize>,
}

/// Reserve `list` at exactly `length` elements, asking the provision for
/// those bytes first; nothing at all for an empty list.
fn reserve_exact<T>(
    provision: &mut Provision<'_>,
    list: &mut Vec<T>,
    length: usize,
) -> Result<(), ReadError> {
    if length == 0 {
        return Ok(());
    }
    let bytes = core::mem::size_of::<T>().saturating_mul(length);
    provision.charge(bytes).map_err(ReadError::from)?;
    list.try_reserve_exact(length)
        .map_err(|_| ReadError::Allocation { requested: bytes })
}

impl Build {
    fn new(width: IndexWidth) -> Self {
        Self {
            text: String::new(),
            ranges: RangeVec::new(width),
            tags: Vec::new(),
            items: IndexVec::new(width),
            members: MemberVec::new(width),
            scratch_items: Vec::new(),
            scratch_members: Vec::new(),
            order: Vec::new(),
        }
    }

    /// A builder whose lists hold exactly what `count` counted, each reserved
    /// once, in the order text, ranges, tags, child ids, members, open-array
    /// children, open-object members, duplicate-sort buffer.
    fn exact(
        count: &Count,
        width: IndexWidth,
        provision: &mut Provision<'_>,
    ) -> Result<Self, ReadError> {
        let mut build = Self::new(width);
        if count.text > 0 {
            provision.charge(count.text).map_err(ReadError::from)?;
            build
                .text
                .try_reserve_exact(count.text)
                .map_err(|_| ReadError::Allocation {
                    requested: count.text,
                })?;
        }
        build.ranges.reserve_exact(provision, count.slots)?;
        reserve_exact(provision, &mut build.tags, count.slots)?;
        build.items.reserve_exact(provision, count.items)?;
        build.members.reserve_exact(provision, count.members)?;
        reserve_exact(
            provision,
            &mut build.scratch_items,
            count.scratch_items_peak,
        )?;
        reserve_exact(
            provision,
            &mut build.scratch_members,
            count.scratch_members_peak,
        )?;
        reserve_exact(provision, &mut build.order, count.order_peak)?;
        Ok(build)
    }

    fn into_document(self, root: usize) -> Document {
        Document {
            text: self.text,
            ranges: self.ranges,
            tags: self.tags,
            items: self.items,
            members: self.members,
            root,
        }
    }

    /// An owned copy of `text`, charged to the reservation and reserved
    /// fallibly.
    fn owned(provision: &mut Provision<'_>, text: &str) -> Result<String, ReadError> {
        let mut copy = String::new();
        provision
            .grow_text(&mut copy, text.len())
            .map_err(ReadError::from)?;
        copy.push_str(text);
        Ok(copy)
    }

    /// The RFC 6901 pointer of the value being read, from the open containers
    /// on `stack`, below the member `name` of the object whose members start
    /// at `children_start` on `scratch_members` when given.
    fn pointer(
        &self,
        provision: &mut Provision<'_>,
        stack: &[OpenContainer],
        below: Option<(Range, usize)>,
    ) -> Result<String, ReadError> {
        enum Step {
            Index(usize),
            Name(Range),
        }
        // Walk innermost out. Open arrays share `scratch_items` and open
        // objects share `scratch_members`, each container's children starting
        // at its `start`, so a container's children end where the next open
        // container of the same kind begins.
        let mut steps = Vec::new();
        let mut items_end = self.scratch_items.len();
        let mut members_end = self.scratch_members.len();
        if let Some((name, children_start)) = below {
            provision.grow(&mut steps, 1).map_err(ReadError::from)?;
            steps.push(Step::Name(name));
            // The object being closed still has its members on the shared
            // list; the ones below it belong to the objects around it.
            members_end = children_start;
        }
        for open in stack.iter().rev() {
            if open.object {
                let member = members_end
                    .checked_sub(1)
                    .and_then(|index| self.scratch_members.get(index));
                if let Some(member) = member {
                    provision.grow(&mut steps, 1).map_err(ReadError::from)?;
                    steps.push(Step::Name(member.name));
                }
                members_end = open.start;
            } else {
                provision.grow(&mut steps, 1).map_err(ReadError::from)?;
                steps.push(Step::Index(items_end.saturating_sub(open.start)));
                items_end = open.start;
            }
        }
        // The exact length of the escaped path: one `/` per step, then the
        // decimal index or the name with `~` and `/` doubled.
        let mut length = 0_usize;
        for step in &steps {
            let part = match step {
                Step::Index(index) => index
                    .checked_ilog10()
                    .map_or(1, |digits| digits as usize + 1),
                Step::Name(range) => {
                    let name = self.text.get(range.start..range.end).unwrap_or_default();
                    name.len()
                        + name
                            .bytes()
                            .filter(|byte| matches!(byte, b'~' | b'/'))
                            .count()
                }
            };
            length = length.saturating_add(1).saturating_add(part);
        }
        let mut pointer = String::new();
        provision
            .grow_text(&mut pointer, length)
            .map_err(ReadError::from)?;
        for step in steps.iter().rev() {
            pointer.push('/');
            match step {
                Step::Index(index) => {
                    use core::fmt::Write as _;
                    // Writing to a String reserved above cannot fail.
                    let _ = write!(pointer, "{index}");
                }
                Step::Name(range) => {
                    let name = self.text.get(range.start..range.end).unwrap_or_default();
                    for character in name.chars() {
                        match character {
                            '~' => pointer.push_str("~0"),
                            '/' => pointer.push_str("~1"),
                            other => pointer.push(other),
                        }
                    }
                }
            }
        }
        Ok(pointer)
    }

    /// Refuse the object whose members start at `start` on
    /// `scratch_members` when two share a name, at the later one's offset.
    fn check_unique_names(
        &mut self,
        provision: &mut Provision<'_>,
        stack: &[OpenContainer],
        start: usize,
    ) -> Result<(), ReadError> {
        let children = self.scratch_members.get(start..).unwrap_or_default();
        if children.len() < 2 {
            return Ok(());
        }
        let text = &self.text;
        let name_of = |index: usize| {
            children
                .get(index)
                .map(|member| {
                    let name = text.get(member.name.start..member.name.end);
                    (name.unwrap_or_default(), member.offset)
                })
                .unwrap_or_default()
        };
        self.order.clear();
        provision
            .grow(&mut self.order, children.len())
            .map_err(ReadError::from)?;
        self.order.extend(0..children.len());
        self.order
            .sort_unstable_by(|left, right| name_of(*left).cmp(&name_of(*right)));
        let mut repeat = None;
        for pair in self.order.windows(2) {
            let [left, right] = pair else { continue };
            let (left_name, _) = name_of(*left);
            let (right_name, later) = name_of(*right);
            if left_name == right_name {
                // Sorted by (name, offset), so `right` is the later one.
                repeat = children.get(*right).map(|member| (member.name, later));
                break;
            }
        }
        let Some((name, offset)) = repeat else {
            return Ok(());
        };
        let pointer = self.pointer(provision, stack, Some((name, start)))?;
        Err(ReadError::DuplicateName { offset, pointer })
    }
}

impl Builder for Build {
    fn text_len(&self) -> usize {
        self.text.len()
    }

    fn append(&mut self, provision: &mut Provision<'_>, piece: &str) -> Result<(), ReadError> {
        provision
            .grow_text(&mut self.text, piece.len())
            .map_err(ReadError::from)?;
        self.text.push_str(piece);
        Ok(())
    }

    fn node(&mut self, provision: &mut Provision<'_>, slot: Slot) -> Result<usize, ReadError> {
        let id = self.ranges.len();
        self.ranges.grow_push(provision, slot.range)?;
        provision.grow(&mut self.tags, 1).map_err(ReadError::from)?;
        self.tags.push(slot.tag);
        Ok(id)
    }

    fn node_count(&self) -> usize {
        self.ranges.len()
    }

    fn children(&self, object: bool) -> usize {
        if object {
            self.scratch_members.len()
        } else {
            self.scratch_items.len()
        }
    }

    fn push_item(&mut self, provision: &mut Provision<'_>, id: usize) -> Result<(), ReadError> {
        provision
            .grow(&mut self.scratch_items, 1)
            .map_err(ReadError::from)?;
        self.scratch_items.push(id);
        Ok(())
    }

    fn push_member(
        &mut self,
        provision: &mut Provision<'_>,
        member: PendingMember,
    ) -> Result<(), ReadError> {
        provision
            .grow(&mut self.scratch_members, 1)
            .map_err(ReadError::from)?;
        self.scratch_members.push(member);
        Ok(())
    }

    fn set_member_value(&mut self, id: usize) -> bool {
        self.scratch_members
            .last_mut()
            .map(|member| member.id = id)
            .is_some()
    }

    fn close(
        &mut self,
        provision: &mut Provision<'_>,
        stack: &[OpenContainer],
        open: OpenContainer,
    ) -> Result<Slot, ReadError> {
        Ok(if open.object {
            self.check_unique_names(provision, stack, open.start)?;
            let pending = self.scratch_members.get(open.start..).unwrap_or_default();
            let start = self.members.len();
            self.members.grow_for(provision, pending.len())?;
            for member in pending {
                self.members
                    .push_reserved(member.name, member.id)
                    .ok_or_else(index_allocation_error)?;
            }
            self.scratch_members.truncate(open.start);
            Slot::object(Range {
                start,
                end: self.members.len(),
            })
        } else {
            let items = self.scratch_items.get(open.start..).unwrap_or_default();
            let start = self.items.len();
            self.items.grow_for(provision, items.len())?;
            for id in items {
                self.items
                    .push_reserved(*id)
                    .ok_or_else(index_allocation_error)?;
            }
            self.scratch_items.truncate(open.start);
            Slot::array(Range {
                start,
                end: self.items.len(),
            })
        })
    }

    fn number_out_of_range(
        &mut self,
        provision: &mut Provision<'_>,
        stack: &[OpenContainer],
        offset: usize,
        literal: &str,
    ) -> ReadError {
        match self
            .pointer(provision, stack, None)
            .and_then(|pointer| Ok((pointer, Self::owned(provision, literal)?)))
        {
            Ok((pointer, lexeme)) => ReadError::NumberOutOfRange {
                offset,
                pointer,
                lexeme,
            },
            Err(error) => error,
        }
    }
}

/// The builder of pass 1 of [`read_exact_with_reservation`]: the lengths
/// [`Build`] would reach, with no heap. Each list length is what `Build`
/// would hold at the same point of the same input, so pass 2 never grows.
#[derive(Default)]
struct Count {
    /// Decoded text bytes.
    text: usize,
    /// Nodes.
    slots: usize,
    /// Array child ids.
    items: usize,
    /// Object members.
    members: usize,
    /// Open-array children now, and at most.
    scratch_items: usize,
    scratch_items_peak: usize,
    /// Open-object members now, and at most.
    scratch_members: usize,
    scratch_members_peak: usize,
    /// The most members of one object whose names were sorted.
    order_peak: usize,
    /// An object of two or more members closed, so its names are unchecked
    /// for duplicates until pass 2.
    names_unchecked: bool,
}

impl Builder for Count {
    fn text_len(&self) -> usize {
        self.text
    }

    fn append(&mut self, _: &mut Provision<'_>, piece: &str) -> Result<(), ReadError> {
        self.text = self.text.saturating_add(piece.len());
        Ok(())
    }

    fn node(&mut self, _: &mut Provision<'_>, _: Slot) -> Result<usize, ReadError> {
        let id = self.slots;
        self.slots = self.slots.saturating_add(1);
        Ok(id)
    }

    fn node_count(&self) -> usize {
        self.slots
    }

    fn children(&self, object: bool) -> usize {
        if object {
            self.scratch_members
        } else {
            self.scratch_items
        }
    }

    fn push_item(&mut self, _: &mut Provision<'_>, _: usize) -> Result<(), ReadError> {
        self.scratch_items = self.scratch_items.saturating_add(1);
        self.scratch_items_peak = self.scratch_items_peak.max(self.scratch_items);
        Ok(())
    }

    fn push_member(&mut self, _: &mut Provision<'_>, _: PendingMember) -> Result<(), ReadError> {
        self.scratch_members = self.scratch_members.saturating_add(1);
        self.scratch_members_peak = self.scratch_members_peak.max(self.scratch_members);
        Ok(())
    }

    fn set_member_value(&mut self, _: usize) -> bool {
        self.scratch_members > 0
    }

    fn close(
        &mut self,
        _: &mut Provision<'_>,
        _: &[OpenContainer],
        open: OpenContainer,
    ) -> Result<Slot, ReadError> {
        Ok(if open.object {
            let width = self.scratch_members.saturating_sub(open.start);
            if width >= 2 {
                self.order_peak = self.order_peak.max(width);
                self.names_unchecked = true;
            }
            let start = self.members;
            self.members = self.members.saturating_add(width);
            self.scratch_members = open.start;
            Slot::object(Range {
                start,
                end: self.members,
            })
        } else {
            let width = self.scratch_items.saturating_sub(open.start);
            let start = self.items;
            self.items = self.items.saturating_add(width);
            self.scratch_items = open.start;
            Slot::array(Range {
                start,
                end: self.items,
            })
        })
    }

    /// A stand-in refusal with no pointer or lexeme (both empty, so nothing
    /// is allocated); pass 2 makes the real one.
    fn number_out_of_range(
        &mut self,
        _: &mut Provision<'_>,
        _: &[OpenContainer],
        offset: usize,
        _: &str,
    ) -> ReadError {
        ReadError::NumberOutOfRange {
            offset,
            pointer: String::new(),
            lexeme: String::new(),
        }
    }
}

struct Parser<'i, 'r, B> {
    provision: Provision<'r>,
    source: &'i str,
    input: &'i [u8],
    position: usize,
    stack: Vec<OpenContainer>,
    builder: B,
}

impl<'i, 'r, B: Builder> Parser<'i, 'r, B> {
    fn new(
        source: &'i str,
        provision: Provision<'r>,
        stack: Vec<OpenContainer>,
        builder: B,
    ) -> Self {
        Self {
            provision,
            source,
            input: source.as_bytes(),
            position: 0,
            stack,
            builder,
        }
    }

    /// Append `piece` to the decoded text, asking the reservation first.
    fn append(&mut self, piece: &str) -> Result<(), ReadError> {
        self.builder.append(&mut self.provision, piece)
    }

    fn malformed(&self, offset: usize, kind: Malformed) -> ReadError {
        ReadError::Malformed { offset, kind }
    }

    fn peek(&self) -> Option<u8> {
        self.input.get(self.position).copied()
    }

    /// The fault for the byte at the current position: unexpected, or the end.
    fn unexpected(&self) -> ReadError {
        let kind = if self.position < self.input.len() {
            Malformed::UnexpectedCharacter
        } else {
            Malformed::UnexpectedEnd
        };
        self.malformed(self.position, kind)
    }

    fn skip_whitespace(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\t' | b'\n' | b'\r')) {
            self.position += 1;
        }
    }

    /// Read the whole input; the root's node id.
    fn run(&mut self) -> Result<usize, ReadError> {
        let mut expect = Expect::Value;
        let root = loop {
            self.skip_whitespace();
            expect = match expect {
                Expect::Value | Expect::ValueOrEnd => {
                    if expect == Expect::ValueOrEnd && self.peek() == Some(b']') {
                        self.position += 1;
                        let id = self.close(false)?;
                        self.complete(id)?
                    } else {
                        self.value()?
                    }
                }
                Expect::NameOrEnd | Expect::Name => {
                    if expect == Expect::NameOrEnd && self.peek() == Some(b'}') {
                        self.position += 1;
                        let id = self.close(true)?;
                        self.complete(id)?
                    } else {
                        self.member_name()?;
                        Expect::Value
                    }
                }
                Expect::Separator => {
                    let Some(open) = self.stack.last() else {
                        break self.builder.node_count().checked_sub(1);
                    };
                    let object = open.object;
                    match self.peek() {
                        Some(b',') => {
                            self.position += 1;
                            if object {
                                Expect::Name
                            } else {
                                Expect::Value
                            }
                        }
                        Some(b']') if !object => {
                            self.position += 1;
                            let id = self.close(false)?;
                            self.complete(id)?
                        }
                        Some(b'}') if object => {
                            self.position += 1;
                            let id = self.close(true)?;
                            self.complete(id)?
                        }
                        _ => return Err(self.unexpected()),
                    }
                }
            };
        };
        self.skip_whitespace();
        if self.position < self.input.len() {
            return Err(self.malformed(self.position, Malformed::TrailingContent));
        }
        // The top-level value is the last node created: a container's node
        // is created when it closes, after all of its children.
        root.ok_or_else(|| self.malformed(self.position, Malformed::UnexpectedEnd))
    }

    /// Read the value starting at the current position.
    fn value(&mut self) -> Result<Expect, ReadError> {
        let start = self.position;
        let Some(byte) = self.peek() else {
            return Err(self.unexpected());
        };
        let slot = match byte {
            b'{' | b'[' => {
                self.position += 1;
                let object = byte == b'{';
                self.provision
                    .grow(&mut self.stack, 1)
                    .map_err(ReadError::from)?;
                self.stack.push(OpenContainer {
                    object,
                    start: self.builder.children(object),
                });
                return Ok(if object {
                    Expect::NameOrEnd
                } else {
                    Expect::ValueOrEnd
                });
            }
            b'"' => Slot::string(self.string()?),
            b't' => self.literal("true", Slot::boolean(true))?,
            b'f' => self.literal("false", Slot::boolean(false))?,
            b'n' => self.literal("null", Slot::null())?,
            b'-' | b'0'..=b'9' => self.number()?,
            _ => return Err(self.malformed(start, Malformed::UnexpectedCharacter)),
        };
        let id = self.node(slot)?;
        self.complete(id)
    }

    fn literal(&mut self, word: &str, slot: Slot) -> Result<Slot, ReadError> {
        let end = self.position.saturating_add(word.len());
        if self.input.get(self.position..end) != Some(word.as_bytes()) {
            return Err(self.malformed(self.position, Malformed::UnexpectedCharacter));
        }
        self.position = end;
        Ok(slot)
    }

    fn node(&mut self, slot: Slot) -> Result<usize, ReadError> {
        self.builder.node(&mut self.provision, slot)
    }

    /// Attach a finished value to its container, or make it the root.
    fn complete(&mut self, id: usize) -> Result<Expect, ReadError> {
        match self.stack.last() {
            Some(OpenContainer { object: true, .. }) => {
                if !self.builder.set_member_value(id) {
                    return Err(ReadError::Malformed {
                        offset: self.position,
                        kind: Malformed::UnexpectedCharacter,
                    });
                }
            }
            Some(OpenContainer { object: false, .. }) => {
                self.builder.push_item(&mut self.provision, id)?;
            }
            None => {}
        }
        Ok(Expect::Separator)
    }

    /// Read `"name"` and the `:` after it, for the innermost object.
    fn member_name(&mut self) -> Result<(), ReadError> {
        if self.peek() != Some(b'"') {
            return Err(self.unexpected());
        }
        let offset = self.position;
        let name = self.string()?;
        self.skip_whitespace();
        if self.peek() != Some(b':') {
            return Err(self.unexpected());
        }
        self.position += 1;
        self.builder.push_member(
            &mut self.provision,
            PendingMember {
                name,
                offset,
                id: 0,
            },
        )
    }

    /// Close the innermost container, whose closing byte has been consumed.
    fn close(&mut self, object: bool) -> Result<usize, ReadError> {
        let closing = self.position.saturating_sub(1);
        let Some(open) = self.stack.pop() else {
            return Err(self.malformed(closing, Malformed::UnexpectedCharacter));
        };
        if open.object != object {
            return Err(self.malformed(closing, Malformed::UnexpectedCharacter));
        }
        let slot = self.builder.close(&mut self.provision, &self.stack, open)?;
        self.node(slot)
    }

    /// Read the string starting at the current `"`, decoding it into the
    /// text.
    fn string(&mut self) -> Result<Range, ReadError> {
        self.position += 1;
        let start = self.builder.text_len();
        loop {
            let run_start = self.position;
            while let Some(byte) = self.peek() {
                if byte == b'"' || byte == b'\\' || byte < 0x20 {
                    break;
                }
                self.position += 1;
            }
            // The run stops at an ASCII byte or the end, so both ends are
            // character boundaries.
            let run = self
                .source
                .get(run_start..self.position)
                .unwrap_or_default();
            self.append(run)?;
            match self.peek() {
                Some(b'"') => {
                    self.position += 1;
                    return Ok(Range {
                        start,
                        end: self.builder.text_len(),
                    });
                }
                Some(b'\\') => self.escape()?,
                Some(_) => return Err(self.malformed(self.position, Malformed::ControlCharacter)),
                None => return Err(self.unexpected()),
            }
        }
    }

    /// Decode the escape at the current `\` into `text`.
    fn escape(&mut self) -> Result<(), ReadError> {
        let offset = self.position;
        let Some(kind) = self.input.get(offset + 1).copied() else {
            self.position = self.input.len();
            return Err(self.unexpected());
        };
        self.position = offset + 2;
        let decoded = match kind {
            b'"' => '"',
            b'\\' => '\\',
            b'/' => '/',
            b'b' => '\u{8}',
            b'f' => '\u{c}',
            b'n' => '\n',
            b'r' => '\r',
            b't' => '\t',
            b'u' => self.unicode_escape(offset)?,
            _ => return Err(self.malformed(offset, Malformed::InvalidEscape)),
        };
        self.append(decoded.encode_utf8(&mut [0; 4]))
    }

    /// Decode `\uXXXX`, and its low half when it is a high surrogate. The
    /// position is just past `\u`; `offset` is the escape's `\`.
    fn unicode_escape(&mut self, offset: usize) -> Result<char, ReadError> {
        let high = self.hex4(offset)?;
        let code = match high {
            0xD800..=0xDBFF => {
                let paired = self.input.get(self.position..self.position + 2) == Some(b"\\u");
                if !paired {
                    return Err(self.malformed(offset, Malformed::LoneSurrogate));
                }
                let low_offset = self.position;
                self.position += 2;
                let low = self.hex4(low_offset)?;
                if !(0xDC00..=0xDFFF).contains(&low) {
                    return Err(self.malformed(offset, Malformed::LoneSurrogate));
                }
                0x10000 + ((u32::from(high) - 0xD800) << 10) + (u32::from(low) - 0xDC00)
            }
            0xDC00..=0xDFFF => return Err(self.malformed(offset, Malformed::LoneSurrogate)),
            _ => u32::from(high),
        };
        char::from_u32(code).ok_or_else(|| self.malformed(offset, Malformed::LoneSurrogate))
    }

    /// Four hex digits at the current position, for the escape at `offset`.
    fn hex4(&mut self, offset: usize) -> Result<u16, ReadError> {
        let digits = self
            .input
            .get(self.position..self.position + 4)
            .ok_or_else(|| self.malformed(offset, Malformed::InvalidEscape))?;
        let mut value: u16 = 0;
        for digit in digits {
            let nibble = char::from(*digit)
                .to_digit(16)
                .ok_or_else(|| self.malformed(offset, Malformed::InvalidEscape))?;
            value = (value << 4) | u16::try_from(nibble).unwrap_or_default();
        }
        self.position += 4;
        Ok(value)
    }

    /// Read the number starting at the current position.
    fn number(&mut self) -> Result<Slot, ReadError> {
        let start = self.position;
        let invalid = |parser: &Self| parser.malformed(start, Malformed::InvalidNumber);
        if self.peek() == Some(b'-') {
            self.position += 1;
        }
        match self.peek() {
            Some(b'0') => self.position += 1,
            Some(b'1'..=b'9') => self.digits(),
            _ => return Err(invalid(self)),
        }
        if self.peek() == Some(b'.') {
            self.position += 1;
            if !matches!(self.peek(), Some(b'0'..=b'9')) {
                return Err(invalid(self));
            }
            self.digits();
        }
        if matches!(self.peek(), Some(b'e' | b'E')) {
            self.position += 1;
            if matches!(self.peek(), Some(b'+' | b'-')) {
                self.position += 1;
            }
            if !matches!(self.peek(), Some(b'0'..=b'9')) {
                return Err(invalid(self));
            }
            self.digits();
        }
        let literal = self.source.get(start..self.position).unwrap_or_default();
        let value: f64 = literal.parse().map_err(|_| invalid(self))?;
        if !value.is_finite() {
            return Err(self.builder.number_out_of_range(
                &mut self.provision,
                &self.stack,
                start,
                literal,
            ));
        }
        let text_start = self.builder.text_len();
        self.append(literal)?;
        Ok(Slot::number(Range {
            start: text_start,
            end: self.builder.text_len(),
        }))
    }

    fn digits(&mut self) {
        while matches!(self.peek(), Some(b'0'..=b'9')) {
            self.position += 1;
        }
    }
}

#[cfg(all(test, target_pointer_width = "64"))]
mod adaptive_index_tests {
    use super::{IndexWidth, Packed40, Range, Range32, Range40};

    #[test]
    fn packed_indices_check_and_round_trip_width_boundaries() {
        for value in [
            0,
            u32::MAX as usize,
            usize::try_from(1_u64 << 32).expect("fits usize"),
        ] {
            let encoded = Packed40::encode(value).expect("fits in 40 bits");
            assert_eq!(encoded.decode(), Some(value));
        }
        assert_eq!(
            Packed40::encode(usize::try_from(Packed40::MAX).unwrap() + 1),
            None
        );
        let narrow = Range {
            start: 0,
            end: u32::MAX as usize,
        };
        assert_eq!(
            Range32::encode(narrow).and_then(Range32::decode),
            Some(narrow)
        );
        let wide = Range {
            start: usize::try_from(1_u64 << 32).expect("fits usize"),
            end: usize::try_from(Packed40::MAX).expect("40-bit maximum fits usize"),
        };
        assert_eq!(Range40::encode(wide).and_then(Range40::decode), Some(wide));
    }

    #[test]
    fn width_selection_uses_the_narrowest_representable_bound() {
        assert_eq!(IndexWidth::for_bound(u32::MAX as usize), IndexWidth::Bits32);
        assert_eq!(
            IndexWidth::for_bound(usize::try_from(1_u64 << 32).expect("fits usize")),
            IndexWidth::Bits40
        );
        assert_eq!(
            IndexWidth::for_bound(usize::try_from(Packed40::MAX).unwrap()),
            IndexWidth::Bits40
        );
    }
}
