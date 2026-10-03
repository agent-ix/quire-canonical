// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Agent-IX
//! [`Encode`] for [`serde_json::Value`], with the `serde_json` feature.
//!
//! A `Value`'s depth follows its input, so it cannot take the serde path: it
//! walks itself with an explicit heap stack and pushes [`Writer`] events, the
//! way [`crate::NodeRef`] does. [`drop_value`] drops one the same way, since
//! serde_json's own `Drop` recurses.

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
/// * A number serde_json holds as an integer (`Number::as_i64` or
///   `Number::as_u64`, from `-2^63` to `2^64 - 1`) goes through
///   [`Writer::integer`], and is refused past `2^53` like any Rust integer.
///   This differs from [`crate::read`], which encodes every JSON number as
///   the double its text denotes: the literal `9007199254740993` is refused
///   here and encodes as `9007199254740992` there. An integer literal past
///   the 64-bit range is a float to serde_json, so it encodes as its double
///   here too (`18446744073709551617` as `18446744073709552000`), unless
///   `arbitrary_precision` is on (below).
/// * A float goes through [`Writer::number`]. Without `arbitrary_precision`
///   serde_json holds no NaN or infinity in a `Value`.
/// * With serde_json's `arbitrary_precision`, which feature unification
///   turns on for every crate in a build once any crate enables it, a
///   `Number` is its literal text. serde_json's own `as_i64`, `as_u64` and
///   `is_f64` still classify it, so 64-bit integers and finite floats encode
///   as they do without it. Two kinds of literal do not: an integer literal
///   past the 64-bit range, which serde_json otherwise parses as a float and
///   this encodes as its double, stays an integer and is refused
///   ([`Error::WideIntegerMagnitudeAboveMaximum`]); and a literal whose
///   double is infinite, such as `1e400`, which serde_json otherwise refuses
///   to parse, is refused ([`Error::NonFiniteNumber`]). So whether a `Value`
///   parsed from such an integer literal encodes depends on the build.
///
/// serde_json's own `Drop`, `Clone`, `PartialEq` and `Debug` for `Value`
/// recurse once per nesting level. Encoding only borrows the value, but a
/// caller holding a deep `Value` must also drop it without recursing:
/// [`drop_value`] does that.
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

/// Drop `value` without recursing, so a value of any depth drops on any
/// thread stack.
///
/// serde_json's own `Drop` for `Value` recurses once per nesting level, so
/// dropping a deep value (one parsed with serde_json's `unbounded_depth`, or
/// built in a loop) overflows the thread stack. This moves each array's
/// elements and each object's member values onto a heap stack before the
/// array or object itself is dropped, so every node is dropped with no
/// children left in it. There is no depth limit.
///
/// The heap stack holds the nodes waiting to be dropped, never more than the
/// value had. It grows like any `Vec`, so running out of memory while it
/// grows aborts, as it would for any other allocation in a destructor.
///
/// ```
/// use serde_json::Value;
///
/// let mut value = Value::Null;
/// for _ in 0..100_000 {
///     value = Value::Array(vec![value]);
/// }
/// quire_canonical::drop_value(value);
/// ```
pub fn drop_value(value: Value) {
    let mut pending = Vec::from([value]);
    while let Some(node) = pending.pop() {
        match node {
            Value::Array(items) => pending.extend(items),
            Value::Object(members) => pending.extend(members.into_iter().map(|(_, member)| member)),
            Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
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
