// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Agent-IX
//! The streaming RFC 8785 encoder: a [`serde::Serializer`] over a [`Sink`].
//!
//! # Streaming and the one buffer it needs
//!
//! Scalars, strings and arrays go straight to their destination. Objects
//! cannot: RFC 8785 orders members by name, and `Serialize` hands members over
//! in whatever order the value holds them. So each open object keeps its
//! members' encoded bytes (name, `:`, value) in a [`Frame`] until it closes,
//! then sorts them by UTF-16 code unit and moves them, comma-separated, to its
//! own destination: the enclosing member's buffer, or the sink at the top
//! level. Nothing else is buffered; there is no `String` of the whole text and
//! no `serde_json::Value`.
//!
//! # The meter
//!
//! Every byte of canonical text is counted once, when it is produced. Bytes
//! moved from a closed object's buffer to its parent are already counted. The
//! count at any point is therefore the length of the canonical text produced
//! so far, and the bytes held in member buffers are a subset of it: the one
//! [`Limits::max_bytes`] ceiling bounds both the output and the sort buffers.
//! Member names are additionally kept unescaped for sorting; an unescaped name
//! is never longer than its escaped form, so they add at most the same again.
//! Every reservation uses `try_reserve`.

use std::fmt;

use serde::ser::{self, Impossible, Serialize};

use crate::escape::escape_fragment;
use crate::number::{exact_double, with_double_text};
use crate::order::cmp_utf16;
use crate::sink::try_extend;
use crate::{Error, LimitExceeded, LimitKind, Limits, Sink};

/// Struct names `serde_json` uses for its private tokens
/// (`arbitrary_precision` numbers, `RawValue`).
const SERDE_JSON_PRIVATE_PREFIX: &str = "$serde_json::private::";

/// One object member being or having been encoded.
#[derive(Default)]
struct Member {
    /// The unescaped name, compared for ordering.
    name: String,
    /// `"name":value`, escaped and canonical.
    bytes: Vec<u8>,
}

/// An object that is open: its finished members and the one being built.
#[derive(Default)]
struct Frame {
    members: Vec<Member>,
    building: Member,
    has_name: bool,
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
            Some(frame) => try_extend(&mut frame.building.bytes, bytes),
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
        let double =
            exact_double(value < 0, value.unsigned_abs()).ok_or(Error::InexactInteger(value))?;
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

    fn begin_member(&mut self, name: &str) -> Result<(), Error> {
        let frame = self.top_frame()?;
        frame.building.name = try_string(name)?;
        frame.has_name = true;
        self.string(name)?;
        self.produce(b":")
    }

    fn end_member(&mut self) -> Result<(), Error> {
        let frame = self.top_frame()?;
        frame.has_name = false;
        frame
            .members
            .try_reserve(1)
            .map_err(|_| allocation_of::<Member>(1))?;
        let member = std::mem::take(&mut frame.building);
        frame.members.push(member);
        Ok(())
    }

    fn close_object(&mut self) -> Result<(), Error> {
        let Some(mut frame) = self.frames.pop() else {
            return Err(Error::Serialize(
                "object closed that was never opened".to_owned(),
            ));
        };
        // Unstable sort: in place, no allocation. Names are unique (checked
        // next), so stability cannot matter.
        frame
            .members
            .sort_unstable_by(|left, right| cmp_utf16(&left.name, &right.name));
        if let Some([duplicate, _]) = frame
            .members
            .windows(2)
            .find(|pair| matches!(pair, [left, right] if left.name == right.name))
        {
            return Err(Error::DuplicateMemberName {
                name: duplicate.name.clone(),
            });
        }
        for (index, member) in frame.members.iter().enumerate() {
            if index > 0 {
                self.produce(b",")?;
            }
            self.emit(&member.bytes)?;
        }
        self.produce(b"}")?;
        self.leave();
        Ok(())
    }

    fn top_frame(&mut self) -> Result<&mut Frame, Error> {
        self.frames
            .last_mut()
            .ok_or_else(|| Error::Serialize("object member written outside an object".to_owned()))
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

fn allocation_of<T>(count: usize) -> Error {
    Error::Allocation {
        requested: std::mem::size_of::<T>().saturating_mul(count),
    }
}

fn try_string(text: &str) -> Result<String, Error> {
    let mut owned = String::new();
    owned
        .try_reserve_exact(text.len())
        .map_err(|_| Error::Allocation {
            requested: text.len(),
        })?;
    owned.push_str(text);
    Ok(owned)
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
        let double = exact_double(false, value).ok_or(Error::InexactUnsignedInteger(value))?;
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
        let name = key.serialize(MemberName)?;
        self.encoder.begin_member(&name)
    }

    fn serialize_value<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<(), Error> {
        if !self.encoder.top_frame()?.has_name {
            return Err(Error::Serialize(
                "SerializeMap::serialize_value called before serialize_key".to_owned(),
            ));
        }
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

/// Turns a map key into a member name. JSON names are strings; like
/// `serde_json`, integer and `char` keys are accepted by their decimal or
/// literal text, and everything else is refused.
struct MemberName;

fn non_string(found: &'static str) -> Error {
    Error::NonStringMemberName { found }
}

impl ser::Serializer for MemberName {
    type Ok = String;
    type Error = Error;
    type SerializeSeq = Impossible<String, Error>;
    type SerializeTuple = Impossible<String, Error>;
    type SerializeTupleStruct = Impossible<String, Error>;
    type SerializeTupleVariant = Impossible<String, Error>;
    type SerializeMap = Impossible<String, Error>;
    type SerializeStruct = Impossible<String, Error>;
    type SerializeStructVariant = Impossible<String, Error>;

    fn serialize_str(self, value: &str) -> Result<String, Error> {
        try_string(value)
    }

    fn serialize_char(self, value: char) -> Result<String, Error> {
        let mut buffer = [0_u8; 4];
        try_string(value.encode_utf8(&mut buffer))
    }

    fn serialize_i8(self, value: i8) -> Result<String, Error> {
        Ok(value.to_string())
    }

    fn serialize_i16(self, value: i16) -> Result<String, Error> {
        Ok(value.to_string())
    }

    fn serialize_i32(self, value: i32) -> Result<String, Error> {
        Ok(value.to_string())
    }

    fn serialize_i64(self, value: i64) -> Result<String, Error> {
        Ok(value.to_string())
    }

    fn serialize_i128(self, value: i128) -> Result<String, Error> {
        Ok(value.to_string())
    }

    fn serialize_u8(self, value: u8) -> Result<String, Error> {
        Ok(value.to_string())
    }

    fn serialize_u16(self, value: u16) -> Result<String, Error> {
        Ok(value.to_string())
    }

    fn serialize_u32(self, value: u32) -> Result<String, Error> {
        Ok(value.to_string())
    }

    fn serialize_u64(self, value: u64) -> Result<String, Error> {
        Ok(value.to_string())
    }

    fn serialize_u128(self, value: u128) -> Result<String, Error> {
        Ok(value.to_string())
    }

    fn serialize_unit_variant(
        self,
        _name: &'static str,
        _index: u32,
        variant: &'static str,
    ) -> Result<String, Error> {
        try_string(variant)
    }

    fn serialize_newtype_struct<T: Serialize + ?Sized>(
        self,
        _name: &'static str,
        value: &T,
    ) -> Result<String, Error> {
        value.serialize(self)
    }

    fn serialize_bool(self, _value: bool) -> Result<String, Error> {
        Err(non_string("bool"))
    }

    fn serialize_f32(self, _value: f32) -> Result<String, Error> {
        Err(non_string("f32"))
    }

    fn serialize_f64(self, _value: f64) -> Result<String, Error> {
        Err(non_string("f64"))
    }

    fn serialize_bytes(self, _value: &[u8]) -> Result<String, Error> {
        Err(non_string("bytes"))
    }

    fn serialize_none(self) -> Result<String, Error> {
        Err(non_string("none"))
    }

    fn serialize_some<T: Serialize + ?Sized>(self, _value: &T) -> Result<String, Error> {
        Err(non_string("some"))
    }

    fn serialize_unit(self) -> Result<String, Error> {
        Err(non_string("unit"))
    }

    fn serialize_unit_struct(self, _name: &'static str) -> Result<String, Error> {
        Err(non_string("unit struct"))
    }

    fn serialize_newtype_variant<T: Serialize + ?Sized>(
        self,
        _name: &'static str,
        _index: u32,
        _variant: &'static str,
        _value: &T,
    ) -> Result<String, Error> {
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
