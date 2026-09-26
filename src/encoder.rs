// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Agent-IX
//! The streaming RFC 8785 encoder: a [`serde::Serializer`] over a [`Sink`].
//!
//! # What streams and what is buffered
//!
//! Scalars, strings and arrays go straight to their destination. Objects
//! cannot: RFC 8785 orders members by name, and `Serialize` hands members over
//! in whatever order the value holds them. So each open object keeps a
//! [`Frame`]: one byte buffer holding its members' canonical bytes
//! (`"name":value`) back to back, and one 8-byte offset pair per member. When
//! the object closes, the offsets are sorted by the names' UTF-16 code units,
//! read straight out of the buffer (no second copy of the names), and the
//! members move, comma-separated, to the object's own destination: the
//! enclosing frame's buffer, or the sink at the top level.
//!
//! Consequently a top-level array streams element by element, but a
//! top-level *object* is held whole until it closes, and the sink sees its
//! first byte only then. There is no `String` of the whole text and no
//! `serde_json::Value`, but there is this buffer.
//!
//! # Bounds
//!
//! Every byte of canonical text is counted once, when it is produced, against
//! [`Limits::max_bytes`]; bytes moved from a closed frame to its parent are not
//! counted again. Frame buffers only ever hold produced bytes, so their total
//! length never exceeds the ceiling. What the ceiling does not count:
//!
//! * the transient second copy while a closing object's members move into its
//!   parent (at most the size of that object);
//! * the 8 bytes of offsets per buffered member;
//! * `Vec` growth slack (amortized doubling, up to the length again).
//!
//! `tests/memory.rs` measures the peak heap of the worst shape, a flat object
//! of small members, and keeps it under 4x the canonical length. Buffers and
//! offset vectors grow with `try_reserve`, so exhausting memory is a refusal,
//! not an abort; the fixed-size frame records and integer map-key text are
//! ordinary allocations.

use std::fmt;

use serde::ser::{self, Impossible, Serialize};

use crate::escape::escape_fragment;
use crate::number::{exact_integer_double, with_double_text};
use crate::order::{cmp_member_names, Unescaped};
use crate::sink::try_extend;
use crate::{Error, LimitExceeded, LimitKind, Limits, ProtocolViolation, Sink};

/// Struct names `serde_json` uses for its private tokens
/// (`arbitrary_precision` numbers, `RawValue`).
const SERDE_JSON_PRIVATE_PREFIX: &str = "$serde_json::private::";

/// Where one member's canonical bytes (`"name":value`) sit in its frame's
/// buffer.
#[derive(Clone, Copy)]
struct Span {
    start: u32,
    end: u32,
}

/// An open object.
#[derive(Default)]
struct Frame {
    /// Finished members' canonical bytes, then the one being written.
    buffer: Vec<u8>,
    /// Finished members, in the order they were written.
    members: Vec<Span>,
    /// Where the member being written starts, once its name is written and
    /// until its value is.
    pending: Option<u32>,
}

/// Encodes one value into a sink. Created and consumed by [`crate::encode`].
pub(crate) struct Encoder<'s, S: Sink + ?Sized> {
    sink: &'s mut S,
    limits: Limits,
    produced: u64,
    depth: u32,
    frames: Vec<Frame>,
}

impl<'s, S: Sink + ?Sized> Encoder<'s, S> {
    pub(crate) fn new(sink: &'s mut S, limits: Limits) -> Self {
        Self {
            sink,
            limits,
            produced: 0,
            depth: 0,
            frames: Vec::new(),
        }
    }

    /// The number of canonical bytes produced.
    pub(crate) fn produced(&self) -> u64 {
        self.produced
    }

    /// Move already-counted bytes to the current destination.
    fn emit(&mut self, bytes: &[u8]) -> Result<(), Error> {
        match self.frames.last_mut() {
            Some(frame) => try_extend(&mut frame.buffer, bytes),
            None => self.sink.write_bytes(bytes),
        }
    }

    /// Count new canonical bytes against the ceiling, then emit them.
    fn produce(&mut self, bytes: &[u8]) -> Result<(), Error> {
        let bound = self.limits.max_bytes();
        let required = u64::try_from(bytes.len())
            .ok()
            .and_then(|length| self.produced.checked_add(length))
            .unwrap_or(u64::MAX);
        if required > bound {
            return Err(LimitExceeded {
                kind: LimitKind::CanonicalBytes,
                bound,
                required,
            }
            .into());
        }
        self.emit(bytes)?;
        self.produced = required;
        Ok(())
    }

    fn enter(&mut self) -> Result<(), Error> {
        let bound = self.limits.max_depth();
        let required = self.depth.saturating_add(1);
        if required > bound {
            return Err(LimitExceeded {
                kind: LimitKind::NestingDepth,
                bound: u64::from(bound),
                required: u64::from(required),
            }
            .into());
        }
        self.depth = required;
        Ok(())
    }

    fn leave(&mut self) {
        self.depth = self.depth.saturating_sub(1);
    }

    fn string(&mut self, text: &str) -> Result<(), Error> {
        self.produce(b"\"")?;
        escape_fragment(text, |run| self.produce(run))?;
        self.produce(b"\"")
    }

    fn double(&mut self, value: f64) -> Result<(), Error> {
        with_double_text(value, |text| self.produce(text))
    }

    fn integer(&mut self, value: i128) -> Result<(), Error> {
        let double = exact_integer_double(value < 0, value.unsigned_abs())
            .ok_or(Error::IntegerMagnitudeAboveMaximum(value))?;
        self.double(double)
    }

    fn open_object(&mut self) -> Result<(), Error> {
        self.enter()?;
        self.produce(b"{")?;
        self.frames
            .try_reserve(1)
            .map_err(|_| allocation_of::<Frame>(1))?;
        self.frames.push(Frame::default());
        Ok(())
    }

    fn top_frame(&mut self) -> Result<&mut Frame, Error> {
        self.frames.last_mut().ok_or(Error::Internal {
            invariant: "object members are only written inside an open object",
        })
    }

    /// Write `"name":` as the start of a new member of the innermost object.
    fn begin_member(&mut self, name: &str) -> Result<(), Error> {
        let frame = self.top_frame()?;
        if frame.pending.is_some() {
            return Err(Error::Protocol(ProtocolViolation::NameWithoutValue));
        }
        frame.pending = Some(buffer_offset(&frame.buffer)?);
        self.string(name)?;
        self.produce(b":")
    }

    /// The member's value has been written; record the finished member.
    fn end_member(&mut self) -> Result<(), Error> {
        let frame = self.top_frame()?;
        let start = frame
            .pending
            .take()
            .ok_or(Error::Protocol(ProtocolViolation::ValueWithoutName))?;
        let end = buffer_offset(&frame.buffer)?;
        frame
            .members
            .try_reserve(1)
            .map_err(|_| allocation_of::<Span>(1))?;
        frame.members.push(Span { start, end });
        Ok(())
    }

    fn close_object(&mut self) -> Result<(), Error> {
        let frame = self.frames.pop().ok_or(Error::Internal {
            invariant: "every object close matches an open",
        })?;
        if frame.pending.is_some() {
            return Err(Error::Protocol(ProtocolViolation::ObjectEndedAfterName));
        }
        let Frame {
            buffer,
            mut members,
            pending: _,
        } = frame;
        let bytes_of = |span: Span| {
            let start = usize::try_from(span.start).ok();
            let end = usize::try_from(span.end).ok();
            start
                .zip(end)
                .and_then(|(start, end)| buffer.get(start..end))
                .ok_or(Error::Internal {
                    invariant: "member spans lie inside their frame buffer",
                })
        };
        // Unstable sort: in place, no allocation. Names are unique (checked
        // next), so stability cannot matter. A span outside the buffer sorts
        // as empty here and is refused below.
        members.sort_unstable_by(|left, right| {
            cmp_member_names(
                bytes_of(*left).unwrap_or_default(),
                bytes_of(*right).unwrap_or_default(),
            )
        });
        for (index, span) in members.iter().enumerate() {
            let member = bytes_of(*span)?;
            if let Some(next) = members.get(index + 1) {
                if cmp_member_names(member, bytes_of(*next)?).is_eq() {
                    let name: Vec<u8> = Unescaped::member_name(member).collect();
                    return Err(Error::DuplicateMemberName {
                        name: String::from_utf8_lossy(&name).into_owned(),
                    });
                }
            }
        }
        for (index, span) in members.iter().enumerate() {
            if index > 0 {
                self.produce(b",")?;
            }
            self.emit(bytes_of(*span)?)?;
        }
        self.produce(b"}")?;
        self.leave();
        Ok(())
    }

    /// `{"variant":` — the wrapper serde's externally tagged enums use.
    fn open_variant(&mut self, variant: &str) -> Result<(), Error> {
        self.enter()?;
        self.produce(b"{")?;
        self.string(variant)?;
        self.produce(b":")
    }

    fn close_variant(&mut self) -> Result<(), Error> {
        self.produce(b"}")?;
        self.leave();
        Ok(())
    }
}

/// The current end of a frame buffer as a 32-bit member offset.
fn buffer_offset(buffer: &[u8]) -> Result<u32, Error> {
    u32::try_from(buffer.len()).map_err(|_| {
        LimitExceeded {
            kind: LimitKind::ObjectBytes,
            bound: u64::from(u32::MAX),
            required: u64::try_from(buffer.len()).unwrap_or(u64::MAX),
        }
        .into()
    })
}

fn allocation_of<T>(count: usize) -> Error {
    Error::Allocation {
        requested: std::mem::size_of::<T>().saturating_mul(count),
    }
}

/// An array (or tuple) in progress.
pub(crate) struct Array<'e, 's, S: Sink + ?Sized> {
    encoder: &'e mut Encoder<'s, S>,
    first: bool,
    in_variant: bool,
}

impl<S: Sink + ?Sized> Array<'_, '_, S> {
    fn element<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<(), Error> {
        if !self.first {
            self.encoder.produce(b",")?;
        }
        self.first = false;
        value.serialize(&mut *self.encoder)
    }

    fn finish(self) -> Result<(), Error> {
        self.encoder.produce(b"]")?;
        self.encoder.leave();
        if self.in_variant {
            self.encoder.close_variant()?;
        }
        Ok(())
    }
}

/// An object (map or struct) in progress.
pub(crate) struct Object<'e, 's, S: Sink + ?Sized> {
    encoder: &'e mut Encoder<'s, S>,
    in_variant: bool,
}

impl<S: Sink + ?Sized> Object<'_, '_, S> {
    fn field<T: Serialize + ?Sized>(&mut self, name: &str, value: &T) -> Result<(), Error> {
        self.encoder.begin_member(name)?;
        value.serialize(&mut *self.encoder)?;
        self.encoder.end_member()
    }

    fn finish(self) -> Result<(), Error> {
        self.encoder.close_object()?;
        if self.in_variant {
            self.encoder.close_variant()?;
        }
        Ok(())
    }
}

impl<'e, 's, S: Sink + ?Sized> Encoder<'s, S> {
    fn array(&'e mut self, in_variant: bool) -> Result<Array<'e, 's, S>, Error> {
        self.enter()?;
        self.produce(b"[")?;
        Ok(Array {
            encoder: self,
            first: true,
            in_variant,
        })
    }

    fn object(&'e mut self, in_variant: bool) -> Result<Object<'e, 's, S>, Error> {
        self.open_object()?;
        Ok(Object {
            encoder: self,
            in_variant,
        })
    }
}

impl<'e, 's, S: Sink + ?Sized> ser::Serializer for &'e mut Encoder<'s, S> {
    type Ok = ();
    type Error = Error;
    type SerializeSeq = Array<'e, 's, S>;
    type SerializeTuple = Array<'e, 's, S>;
    type SerializeTupleStruct = Array<'e, 's, S>;
    type SerializeTupleVariant = Array<'e, 's, S>;
    type SerializeMap = Object<'e, 's, S>;
    type SerializeStruct = Object<'e, 's, S>;
    type SerializeStructVariant = Object<'e, 's, S>;

    fn serialize_bool(self, value: bool) -> Result<(), Error> {
        self.produce(if value { b"true" } else { b"false" })
    }

    fn serialize_i8(self, value: i8) -> Result<(), Error> {
        self.integer(i128::from(value))
    }

    fn serialize_i16(self, value: i16) -> Result<(), Error> {
        self.integer(i128::from(value))
    }

    fn serialize_i32(self, value: i32) -> Result<(), Error> {
        self.integer(i128::from(value))
    }

    fn serialize_i64(self, value: i64) -> Result<(), Error> {
        self.integer(i128::from(value))
    }

    fn serialize_i128(self, value: i128) -> Result<(), Error> {
        self.integer(value)
    }

    fn serialize_u8(self, value: u8) -> Result<(), Error> {
        self.integer(i128::from(value))
    }

    fn serialize_u16(self, value: u16) -> Result<(), Error> {
        self.integer(i128::from(value))
    }

    fn serialize_u32(self, value: u32) -> Result<(), Error> {
        self.integer(i128::from(value))
    }

    fn serialize_u64(self, value: u64) -> Result<(), Error> {
        self.integer(i128::from(value))
    }

    fn serialize_u128(self, value: u128) -> Result<(), Error> {
        let double = exact_integer_double(false, value)
            .ok_or(Error::UnsignedIntegerMagnitudeAboveMaximum(value))?;
        self.double(double)
    }

    fn serialize_f32(self, value: f32) -> Result<(), Error> {
        self.double(f64::from(value))
    }

    fn serialize_f64(self, value: f64) -> Result<(), Error> {
        self.double(value)
    }

    fn serialize_char(self, value: char) -> Result<(), Error> {
        let mut buffer = [0_u8; 4];
        self.string(value.encode_utf8(&mut buffer))
    }

    fn serialize_str(self, value: &str) -> Result<(), Error> {
        self.string(value)
    }

    /// Bytes have no JSON type; like `serde_json`, encode them as an array of
    /// numbers.
    fn serialize_bytes(self, value: &[u8]) -> Result<(), Error> {
        let mut array = self.array(false)?;
        for byte in value {
            array.element(byte)?;
        }
        array.finish()
    }

    fn serialize_none(self) -> Result<(), Error> {
        self.produce(b"null")
    }

    fn serialize_some<T: Serialize + ?Sized>(self, value: &T) -> Result<(), Error> {
        value.serialize(self)
    }

    fn serialize_unit(self) -> Result<(), Error> {
        self.produce(b"null")
    }

    fn serialize_unit_struct(self, _name: &'static str) -> Result<(), Error> {
        self.produce(b"null")
    }

    fn serialize_unit_variant(
        self,
        _name: &'static str,
        _index: u32,
        variant: &'static str,
    ) -> Result<(), Error> {
        self.string(variant)
    }

    fn serialize_newtype_struct<T: Serialize + ?Sized>(
        self,
        _name: &'static str,
        value: &T,
    ) -> Result<(), Error> {
        value.serialize(self)
    }

    fn serialize_newtype_variant<T: Serialize + ?Sized>(
        self,
        _name: &'static str,
        _index: u32,
        variant: &'static str,
        value: &T,
    ) -> Result<(), Error> {
        self.open_variant(variant)?;
        value.serialize(&mut *self)?;
        self.close_variant()
    }

    fn serialize_seq(self, _len: Option<usize>) -> Result<Self::SerializeSeq, Error> {
        self.array(false)
    }

    fn serialize_tuple(self, _len: usize) -> Result<Self::SerializeTuple, Error> {
        self.array(false)
    }

    fn serialize_tuple_struct(
        self,
        _name: &'static str,
        _len: usize,
    ) -> Result<Self::SerializeTupleStruct, Error> {
        self.array(false)
    }

    fn serialize_tuple_variant(
        self,
        _name: &'static str,
        _index: u32,
        variant: &'static str,
        _len: usize,
    ) -> Result<Self::SerializeTupleVariant, Error> {
        self.open_variant(variant)?;
        self.array(true)
    }

    fn serialize_map(self, _len: Option<usize>) -> Result<Self::SerializeMap, Error> {
        self.object(false)
    }

    fn serialize_struct(
        self,
        name: &'static str,
        _len: usize,
    ) -> Result<Self::SerializeStruct, Error> {
        if name.starts_with(SERDE_JSON_PRIVATE_PREFIX) {
            return Err(Error::SerdeJsonPrivateToken(name));
        }
        self.object(false)
    }

    fn serialize_struct_variant(
        self,
        _name: &'static str,
        _index: u32,
        variant: &'static str,
        _len: usize,
    ) -> Result<Self::SerializeStructVariant, Error> {
        self.open_variant(variant)?;
        self.object(true)
    }

    /// Escape `Display` output straight into the meter, with no intermediate
    /// `String`.
    fn collect_str<T: fmt::Display + ?Sized>(self, value: &T) -> Result<(), Error> {
        struct Escaper<'a, 'e, 's, S: Sink + ?Sized> {
            encoder: &'a mut &'e mut Encoder<'s, S>,
            failure: Option<Error>,
        }
        impl<S: Sink + ?Sized> fmt::Write for Escaper<'_, '_, '_, S> {
            fn write_str(&mut self, text: &str) -> fmt::Result {
                escape_fragment(text, |run| self.encoder.produce(run)).map_err(|error| {
                    self.failure = Some(error);
                    fmt::Error
                })
            }
        }

        let mut encoder = self;
        encoder.produce(b"\"")?;
        let mut escaper = Escaper {
            encoder: &mut encoder,
            failure: None,
        };
        if fmt::write(&mut escaper, format_args!("{value}")).is_err() {
            return Err(escaper.failure.take().unwrap_or_else(|| {
                Error::Serialize("Display implementation returned an error".to_owned())
            }));
        }
        encoder.produce(b"\"")
    }
}

impl<S: Sink + ?Sized> ser::SerializeSeq for Array<'_, '_, S> {
    type Ok = ();
    type Error = Error;

    fn serialize_element<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<(), Error> {
        self.element(value)
    }

    fn end(self) -> Result<(), Error> {
        self.finish()
    }
}

impl<S: Sink + ?Sized> ser::SerializeTuple for Array<'_, '_, S> {
    type Ok = ();
    type Error = Error;

    fn serialize_element<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<(), Error> {
        self.element(value)
    }

    fn end(self) -> Result<(), Error> {
        self.finish()
    }
}

impl<S: Sink + ?Sized> ser::SerializeTupleStruct for Array<'_, '_, S> {
    type Ok = ();
    type Error = Error;

    fn serialize_field<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<(), Error> {
        self.element(value)
    }

    fn end(self) -> Result<(), Error> {
        self.finish()
    }
}

impl<S: Sink + ?Sized> ser::SerializeTupleVariant for Array<'_, '_, S> {
    type Ok = ();
    type Error = Error;

    fn serialize_field<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<(), Error> {
        self.element(value)
    }

    fn end(self) -> Result<(), Error> {
        self.finish()
    }
}

impl<S: Sink + ?Sized> ser::SerializeMap for Object<'_, '_, S> {
    type Ok = ();
    type Error = Error;

    fn serialize_key<T: Serialize + ?Sized>(&mut self, key: &T) -> Result<(), Error> {
        key.serialize(MemberName {
            encoder: &mut *self.encoder,
        })
    }

    fn serialize_value<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<(), Error> {
        value.serialize(&mut *self.encoder)?;
        self.encoder.end_member()
    }

    fn end(self) -> Result<(), Error> {
        self.finish()
    }
}

impl<S: Sink + ?Sized> ser::SerializeStruct for Object<'_, '_, S> {
    type Ok = ();
    type Error = Error;

    fn serialize_field<T: Serialize + ?Sized>(
        &mut self,
        name: &'static str,
        value: &T,
    ) -> Result<(), Error> {
        self.field(name, value)
    }

    fn end(self) -> Result<(), Error> {
        self.finish()
    }
}

impl<S: Sink + ?Sized> ser::SerializeStructVariant for Object<'_, '_, S> {
    type Ok = ();
    type Error = Error;

    fn serialize_field<T: Serialize + ?Sized>(
        &mut self,
        name: &'static str,
        value: &T,
    ) -> Result<(), Error> {
        self.field(name, value)
    }

    fn end(self) -> Result<(), Error> {
        self.finish()
    }
}

/// Turns a map key into a member name and writes it, with no intermediate
/// copy for string keys. JSON names are strings; like `serde_json`, integer
/// and `char` keys are accepted by their decimal or literal text, and
/// everything else is refused.
struct MemberName<'a, 's, S: Sink + ?Sized> {
    encoder: &'a mut Encoder<'s, S>,
}

impl<S: Sink + ?Sized> MemberName<'_, '_, S> {
    fn decimal(self, value: impl fmt::Display) -> Result<(), Error> {
        // 40 bytes holds any i128/u128 in decimal.
        let mut text = DecimalText::default();
        fmt::write(&mut text, format_args!("{value}")).map_err(|_| Error::Internal {
            invariant: "integer decimal text fits 40 bytes",
        })?;
        let name = text.as_str().ok_or(Error::Internal {
            invariant: "integer decimal text is ASCII",
        })?;
        self.encoder.begin_member(name)
    }
}

/// A fixed stack buffer for an integer's decimal text.
struct DecimalText {
    bytes: [u8; 40],
    length: usize,
}

impl Default for DecimalText {
    fn default() -> Self {
        Self {
            bytes: [0; 40],
            length: 0,
        }
    }
}

impl DecimalText {
    fn as_str(&self) -> Option<&str> {
        std::str::from_utf8(self.bytes.get(..self.length)?).ok()
    }
}

impl fmt::Write for DecimalText {
    fn write_str(&mut self, text: &str) -> fmt::Result {
        let end = self.length.checked_add(text.len()).ok_or(fmt::Error)?;
        self.bytes
            .get_mut(self.length..end)
            .ok_or(fmt::Error)?
            .copy_from_slice(text.as_bytes());
        self.length = end;
        Ok(())
    }
}

fn non_string(found: &'static str) -> Error {
    Error::NonStringMemberName { found }
}

impl<S: Sink + ?Sized> ser::Serializer for MemberName<'_, '_, S> {
    type Ok = ();
    type Error = Error;
    type SerializeSeq = Impossible<(), Error>;
    type SerializeTuple = Impossible<(), Error>;
    type SerializeTupleStruct = Impossible<(), Error>;
    type SerializeTupleVariant = Impossible<(), Error>;
    type SerializeMap = Impossible<(), Error>;
    type SerializeStruct = Impossible<(), Error>;
    type SerializeStructVariant = Impossible<(), Error>;

    fn serialize_str(self, value: &str) -> Result<(), Error> {
        self.encoder.begin_member(value)
    }

    fn serialize_char(self, value: char) -> Result<(), Error> {
        let mut buffer = [0_u8; 4];
        self.encoder.begin_member(value.encode_utf8(&mut buffer))
    }

    fn serialize_i8(self, value: i8) -> Result<(), Error> {
        self.decimal(value)
    }

    fn serialize_i16(self, value: i16) -> Result<(), Error> {
        self.decimal(value)
    }

    fn serialize_i32(self, value: i32) -> Result<(), Error> {
        self.decimal(value)
    }

    fn serialize_i64(self, value: i64) -> Result<(), Error> {
        self.decimal(value)
    }

    fn serialize_i128(self, value: i128) -> Result<(), Error> {
        self.decimal(value)
    }

    fn serialize_u8(self, value: u8) -> Result<(), Error> {
        self.decimal(value)
    }

    fn serialize_u16(self, value: u16) -> Result<(), Error> {
        self.decimal(value)
    }

    fn serialize_u32(self, value: u32) -> Result<(), Error> {
        self.decimal(value)
    }

    fn serialize_u64(self, value: u64) -> Result<(), Error> {
        self.decimal(value)
    }

    fn serialize_u128(self, value: u128) -> Result<(), Error> {
        self.decimal(value)
    }

    fn serialize_unit_variant(
        self,
        _name: &'static str,
        _index: u32,
        variant: &'static str,
    ) -> Result<(), Error> {
        self.encoder.begin_member(variant)
    }

    fn serialize_newtype_struct<T: Serialize + ?Sized>(
        self,
        _name: &'static str,
        value: &T,
    ) -> Result<(), Error> {
        value.serialize(self)
    }

    fn serialize_bool(self, _value: bool) -> Result<(), Error> {
        Err(non_string("bool"))
    }

    fn serialize_f32(self, _value: f32) -> Result<(), Error> {
        Err(non_string("f32"))
    }

    fn serialize_f64(self, _value: f64) -> Result<(), Error> {
        Err(non_string("f64"))
    }

    fn serialize_bytes(self, _value: &[u8]) -> Result<(), Error> {
        Err(non_string("bytes"))
    }

    fn serialize_none(self) -> Result<(), Error> {
        Err(non_string("none"))
    }

    fn serialize_some<T: Serialize + ?Sized>(self, _value: &T) -> Result<(), Error> {
        Err(non_string("some"))
    }

    fn serialize_unit(self) -> Result<(), Error> {
        Err(non_string("unit"))
    }

    fn serialize_unit_struct(self, _name: &'static str) -> Result<(), Error> {
        Err(non_string("unit struct"))
    }

    fn serialize_newtype_variant<T: Serialize + ?Sized>(
        self,
        _name: &'static str,
        _index: u32,
        _variant: &'static str,
        _value: &T,
    ) -> Result<(), Error> {
        Err(non_string("newtype variant"))
    }

    fn serialize_seq(self, _len: Option<usize>) -> Result<Self::SerializeSeq, Error> {
        Err(non_string("sequence"))
    }

    fn serialize_tuple(self, _len: usize) -> Result<Self::SerializeTuple, Error> {
        Err(non_string("tuple"))
    }

    fn serialize_tuple_struct(
        self,
        _name: &'static str,
        _len: usize,
    ) -> Result<Self::SerializeTupleStruct, Error> {
        Err(non_string("tuple struct"))
    }

    fn serialize_tuple_variant(
        self,
        _name: &'static str,
        _index: u32,
        _variant: &'static str,
        _len: usize,
    ) -> Result<Self::SerializeTupleVariant, Error> {
        Err(non_string("tuple variant"))
    }

    fn serialize_map(self, _len: Option<usize>) -> Result<Self::SerializeMap, Error> {
        Err(non_string("map"))
    }

    fn serialize_struct(
        self,
        _name: &'static str,
        _len: usize,
    ) -> Result<Self::SerializeStruct, Error> {
        Err(non_string("struct"))
    }

    fn serialize_struct_variant(
        self,
        _name: &'static str,
        _index: u32,
        _variant: &'static str,
        _len: usize,
    ) -> Result<Self::SerializeStructVariant, Error> {
        Err(non_string("struct variant"))
    }
}
