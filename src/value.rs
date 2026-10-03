// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Agent-IX
//! [`Encode`] for [`serde_json::Value`], with the `serde_json` feature.
//!
//! A `Value`'s depth follows its input, so it cannot take the serde path: it
//! walks itself with an explicit heap stack and pushes [`Writer`] events, the
//! way [`crate::NodeRef`] does.

use alloc::string::ToString as _;
use alloc::vec::Vec;

use serde_json::{map, Number, Value};

use crate::{Encode, Error, Sink, Writer};

/// An open array or object of the walk: its elements or members not yet
/// pushed.
enum Open<'v> {
    Array(core::slice::Iter<'v, Value>),
    Object(map::Iter<'v>),
}

/// This value's RFC 8785 text, written from an explicit heap stack with one
/// entry per open array or object, so a value of any depth encodes on any
/// thread stack. Each entry stands for a `[` or `{` already produced, so the
/// stack is bounded by [`crate::Limits::max_bytes`] as the writer's own
/// stacks are.
///
/// * Members are pushed in the order the [`serde_json::Map`] iterates, which
///   depends on serde_json's `preserve_order` feature. The [`Writer`] sorts
///   them by UTF-16 code unit, so the bytes do not depend on it.
/// * An integer (`Number::as_i64` or `Number::as_u64`) goes through
///   [`Writer::integer`], and is refused past `2^53` like any Rust integer.
///   A float goes through [`Writer::number`]. serde_json holds no NaN or
///   infinity in a `Value`.
/// * With serde_json's `arbitrary_precision` feature a `Number` is its
///   literal text, classified by serde_json's own `as_i64`, `as_u64` and
///   `is_f64`, so the bytes and refusals are the same as without it. Two
///   texts exist only in that mode, and both are refused: an integer too wide
///   for 64 bits ([`Error::WideIntegerMagnitudeAboveMaximum`]) and a literal
///   whose double is infinite, such as `1e400` ([`Error::NonFiniteNumber`]).
///
/// serde_json's own `Drop`, `Clone`, `PartialEq` and `Debug` for `Value`
/// recurse once per nesting level. Encoding only borrows the value, but a
/// caller holding a deep `Value` must also drop it without recursing, for
/// example by moving its children onto a heap stack before each node is
/// dropped.
impl Encode for Value {
    fn encode_into<S: Sink + ?Sized>(&self, writer: &mut Writer<'_, S>) -> Result<(), Error> {
        let mut stack: Vec<Open<'_>> = Vec::new();
        let mut value = self;
        loop {
            let open = match value {
                Value::Null => {
                    writer.null()?;
                    None
                }
                Value::Bool(flag) => {
                    writer.bool(*flag)?;
                    None
                }
                Value::Number(number) => {
                    encode_number(writer, number)?;
                    None
                }
                Value::String(text) => {
                    writer.string(text)?;
                    None
                }
                Value::Array(items) => {
                    writer.begin_array()?;
                    Some(Open::Array(items.iter()))
                }
                Value::Object(members) => {
                    writer.begin_object()?;
                    Some(Open::Object(members.iter()))
                }
            };
            if let Some(open) = open {
                if stack.try_reserve(1).is_err() {
                    return writer.refuse(Error::Allocation {
                        requested: core::mem::size_of::<Open<'_>>(),
                    });
                }
                stack.push(open);
            }
            // Move to the next value: the innermost open container's next
            // element or member, closing each container that has none left.
            loop {
                let Some(innermost) = stack.last_mut() else {
                    return Ok(());
                };
                match innermost {
                    Open::Array(items) => {
                        if let Some(item) = items.next() {
                            value = item;
                            break;
                        }
                        writer.end_array()?;
                    }
                    Open::Object(members) => {
                        if let Some((name, member)) = members.next() {
                            writer.name(name)?;
                            value = member;
                            break;
                        }
                        writer.end_object()?;
                    }
                }
                stack.pop();
            }
        }
    }
}

/// An integer through the integer rules, a float through the double rules,
/// as serde_json classifies the number.
fn encode_number<S: Sink + ?Sized>(
    writer: &mut Writer<'_, S>,
    number: &Number,
) -> Result<(), Error> {
    if let Some(integer) = number.as_i64() {
        return writer.integer(i128::from(integer));
    }
    if let Some(integer) = number.as_u64() {
        return writer.integer(i128::from(integer));
    }
    if number.is_f64() {
        if let Some(double) = number.as_f64() {
            return writer.number(double);
        }
    }
    // Only serde_json's `arbitrary_precision` reaches here: the number is
    // its literal text, and is neither a 64-bit integer nor a finite double.
    let text = number.to_string();
    if text.contains(['.', 'e', 'E']) {
        // A float literal whose double is infinite: the double path refuses
        // it as non-finite.
        writer.number(text.parse().unwrap_or(f64::NAN))
    } else {
        writer.refuse(Error::WideIntegerMagnitudeAboveMaximum(text))
    }
}
