// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Agent-IX
//! The fixed-depth serde path: a [`serde::Serializer`] that turns a
//! [`FixedShape`] value's serde calls into [`Writer`] events.
//!
//! serde drives serialization by recursion, one set of native stack frames
//! per nesting level. That is safe only because the value's depth is fixed by
//! its type ([`FixedShape`]); a value whose depth follows its input is written
//! from the caller's own explicit stack through the [`Writer`] event API
//! instead. The serializer itself keeps no state: every open container lives
//! on the writer's heap stack.

use alloc::borrow::ToOwned as _;
use core::fmt;

use serde::ser::{self, Impossible, Serialize};

use crate::escape::escape_fragment;
use crate::{Error, FixedShape, Sink, Writer};

/// Struct names `serde_json` uses for its private tokens
/// (`arbitrary_precision` numbers, `RawValue`).
const SERDE_JSON_PRIVATE_PREFIX: &str = "$serde_json::private::";

/// Write `value` through its serde encoding.
pub(crate) fn serialize<S, T>(writer: &mut Writer<'_, S>, value: &T) -> Result<(), Error>
where
    S: Sink + ?Sized,
    T: FixedShape + ?Sized,
{
    // Evaluating the type's depth at compile time is what refuses a
    // recursive type: its `DEPTH` refers to itself, and rustc rejects the
    // cycle.
    let _: usize = const { T::DEPTH };
    value.serialize(Ser(writer))
}

/// The serializer for one value; also its own sequence and map state, since
/// the open containers live on the writer.
struct Ser<'w, 's, S: Sink + ?Sized>(&'w mut Writer<'s, S>);

impl<S: Sink + ?Sized> Ser<'_, '_, S> {
    fn nested<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<(), Error> {
        value.serialize(Ser(&mut *self.0))
    }
}

impl<'w, 's, S: Sink + ?Sized> ser::Serializer for Ser<'w, 's, S> {
    type Ok = ();
    type Error = Error;
    type SerializeSeq = Self;
    type SerializeTuple = Self;
    type SerializeTupleStruct = Self;
    type SerializeTupleVariant = Self;
    type SerializeMap = Self;
    type SerializeStruct = Self;
    type SerializeStructVariant = Self;

    fn serialize_bool(self, value: bool) -> Result<(), Error> {
        self.0.bool(value)
    }

    fn serialize_i8(self, value: i8) -> Result<(), Error> {
        self.0.integer(i128::from(value))
    }

    fn serialize_i16(self, value: i16) -> Result<(), Error> {
        self.0.integer(i128::from(value))
    }

    fn serialize_i32(self, value: i32) -> Result<(), Error> {
        self.0.integer(i128::from(value))
    }

    fn serialize_i64(self, value: i64) -> Result<(), Error> {
        self.0.integer(i128::from(value))
    }

    fn serialize_i128(self, value: i128) -> Result<(), Error> {
        self.0.integer(value)
    }

    fn serialize_u8(self, value: u8) -> Result<(), Error> {
        self.0.integer(i128::from(value))
    }

    fn serialize_u16(self, value: u16) -> Result<(), Error> {
        self.0.integer(i128::from(value))
    }

    fn serialize_u32(self, value: u32) -> Result<(), Error> {
        self.0.integer(i128::from(value))
    }

    fn serialize_u64(self, value: u64) -> Result<(), Error> {
        self.0.integer(i128::from(value))
    }

    fn serialize_u128(self, value: u128) -> Result<(), Error> {
        self.0.scalar(|writer| writer.unsigned(value))
    }

    fn serialize_f32(self, value: f32) -> Result<(), Error> {
        self.0.number(f64::from(value))
    }

    fn serialize_f64(self, value: f64) -> Result<(), Error> {
        self.0.number(value)
    }

    fn serialize_char(self, value: char) -> Result<(), Error> {
        let mut buffer = [0_u8; 4];
        self.0.string(value.encode_utf8(&mut buffer))
    }

    fn serialize_str(self, value: &str) -> Result<(), Error> {
        self.0.string(value)
    }

    /// Bytes have no JSON type; like `serde_json`, encode them as an array of
    /// numbers.
    fn serialize_bytes(self, value: &[u8]) -> Result<(), Error> {
        self.0.begin_array()?;
        for byte in value {
            self.0.integer(i128::from(*byte))?;
        }
        self.0.end_array()
    }

    fn serialize_none(self) -> Result<(), Error> {
        self.0.null()
    }

    fn serialize_some<T: Serialize + ?Sized>(self, value: &T) -> Result<(), Error> {
        value.serialize(self)
    }

    fn serialize_unit(self) -> Result<(), Error> {
        self.0.null()
    }

    fn serialize_unit_struct(self, _name: &'static str) -> Result<(), Error> {
        self.0.null()
    }

    fn serialize_unit_variant(
        self,
        _name: &'static str,
        _index: u32,
        variant: &'static str,
    ) -> Result<(), Error> {
        self.0.string(variant)
    }

    fn serialize_newtype_struct<T: Serialize + ?Sized>(
        self,
        _name: &'static str,
        value: &T,
    ) -> Result<(), Error> {
        value.serialize(self)
    }

    fn serialize_newtype_variant<T: Serialize + ?Sized>(
        mut self,
        _name: &'static str,
        _index: u32,
        variant: &'static str,
        value: &T,
    ) -> Result<(), Error> {
        self.0.begin_wrapper(variant)?;
        self.nested(value)?;
        self.0.end_wrapper()
    }

    fn serialize_seq(self, _len: Option<usize>) -> Result<Self, Error> {
        self.0.begin_array()?;
        Ok(self)
    }

    fn serialize_tuple(self, _len: usize) -> Result<Self, Error> {
        self.0.begin_array()?;
        Ok(self)
    }

    fn serialize_tuple_struct(self, _name: &'static str, _len: usize) -> Result<Self, Error> {
        self.0.begin_array()?;
        Ok(self)
    }

    fn serialize_tuple_variant(
        self,
        _name: &'static str,
        _index: u32,
        variant: &'static str,
        _len: usize,
    ) -> Result<Self, Error> {
        self.0.begin_wrapper(variant)?;
        self.0.begin_array()?;
        Ok(self)
    }

    fn serialize_map(self, _len: Option<usize>) -> Result<Self, Error> {
        self.0.begin_object()?;
        Ok(self)
    }

    fn serialize_struct(self, name: &'static str, _len: usize) -> Result<Self, Error> {
        if name.starts_with(SERDE_JSON_PRIVATE_PREFIX) {
            return Err(Error::SerdeJsonPrivateToken(name));
        }
        self.0.begin_object()?;
        Ok(self)
    }

    fn serialize_struct_variant(
        self,
        _name: &'static str,
        _index: u32,
        variant: &'static str,
        _len: usize,
    ) -> Result<Self, Error> {
        self.0.begin_wrapper(variant)?;
        self.0.begin_object()?;
        Ok(self)
    }

    /// Escape `Display` output straight into the meter, with no intermediate
    /// `String`.
    fn collect_str<T: fmt::Display + ?Sized>(self, value: &T) -> Result<(), Error> {
        struct Escaper<'a, 'w, S: Sink + ?Sized> {
            writer: &'a mut Writer<'w, S>,
            failure: Option<Error>,
        }
        impl<S: Sink + ?Sized> fmt::Write for Escaper<'_, '_, S> {
            fn write_str(&mut self, text: &str) -> fmt::Result {
                escape_fragment(text, |run| self.writer.produce(run)).map_err(|error| {
                    self.failure = Some(error);
                    fmt::Error
                })
            }
        }

        self.0.scalar(|writer| {
            writer.produce(b"\"")?;
            let mut escaper = Escaper {
                writer: &mut *writer,
                failure: None,
            };
            if fmt::write(&mut escaper, format_args!("{value}")).is_err() {
                return Err(escaper.failure.take().unwrap_or_else(|| {
                    Error::Serialize("Display implementation returned an error".to_owned())
                }));
            }
            writer.produce(b"\"")
        })
    }
}

impl<S: Sink + ?Sized> ser::SerializeSeq for Ser<'_, '_, S> {
    type Ok = ();
    type Error = Error;

    fn serialize_element<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<(), Error> {
        self.nested(value)
    }

    fn end(self) -> Result<(), Error> {
        self.0.end_array()
    }
}

impl<S: Sink + ?Sized> ser::SerializeTuple for Ser<'_, '_, S> {
    type Ok = ();
    type Error = Error;

    fn serialize_element<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<(), Error> {
        self.nested(value)
    }

    fn end(self) -> Result<(), Error> {
        self.0.end_array()
    }
}

impl<S: Sink + ?Sized> ser::SerializeTupleStruct for Ser<'_, '_, S> {
    type Ok = ();
    type Error = Error;

    fn serialize_field<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<(), Error> {
        self.nested(value)
    }

    fn end(self) -> Result<(), Error> {
        self.0.end_array()
    }
}

impl<S: Sink + ?Sized> ser::SerializeTupleVariant for Ser<'_, '_, S> {
    type Ok = ();
    type Error = Error;

    fn serialize_field<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<(), Error> {
        self.nested(value)
    }

    fn end(self) -> Result<(), Error> {
        self.0.end_array()?;
        self.0.end_wrapper()
    }
}

impl<S: Sink + ?Sized> ser::SerializeMap for Ser<'_, '_, S> {
    type Ok = ();
    type Error = Error;

    fn serialize_key<T: Serialize + ?Sized>(&mut self, key: &T) -> Result<(), Error> {
        key.serialize(MemberName {
            writer: &mut *self.0,
        })
    }

    fn serialize_value<T: Serialize + ?Sized>(&mut self, value: &T) -> Result<(), Error> {
        self.nested(value)
    }

    fn end(self) -> Result<(), Error> {
        self.0.end_object()
    }
}

impl<S: Sink + ?Sized> ser::SerializeStruct for Ser<'_, '_, S> {
    type Ok = ();
    type Error = Error;

    fn serialize_field<T: Serialize + ?Sized>(
        &mut self,
        name: &'static str,
        value: &T,
    ) -> Result<(), Error> {
        self.0.name(name)?;
        self.nested(value)
    }

    fn end(self) -> Result<(), Error> {
        self.0.end_object()
    }
}

impl<S: Sink + ?Sized> ser::SerializeStructVariant for Ser<'_, '_, S> {
    type Ok = ();
    type Error = Error;

    fn serialize_field<T: Serialize + ?Sized>(
        &mut self,
        name: &'static str,
        value: &T,
    ) -> Result<(), Error> {
        self.0.name(name)?;
        self.nested(value)
    }

    fn end(self) -> Result<(), Error> {
        self.0.end_object()?;
        self.0.end_wrapper()
    }
}

/// Turns a map key into a member name and writes it, with no intermediate
/// copy for string keys. JSON names are strings; like `serde_json`, integer
/// and `char` keys are accepted by their decimal or literal text, and
/// everything else is refused.
struct MemberName<'a, 's, S: Sink + ?Sized> {
    writer: &'a mut Writer<'s, S>,
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
        core::str::from_utf8(self.bytes.get(..self.length)?).ok()
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
        self.writer.name(name)
    }
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
        self.writer.name(value)
    }

    fn serialize_char(self, value: char) -> Result<(), Error> {
        let mut buffer = [0_u8; 4];
        self.writer.name(value.encode_utf8(&mut buffer))
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
        self.writer.name(variant)
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
