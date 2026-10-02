// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Agent-IX
//! The push-event canonical writer: the one RFC 8785 encoder every path in
//! this crate drives.
//!
//! The caller pushes events (`begin_array`, `name`, `string`, `end_object`,
//! ...) one at a time, from whatever explicit stack it walks its own data
//! with. The writer never recurses: each event does a bounded amount of work
//! against its own heap stack of open containers, so a value of any depth
//! encodes on any thread stack.
//!
//! # What streams and what is buffered
//!
//! Scalars, strings and arrays go straight to the sink while no object is
//! open. Objects cannot: RFC 8785 orders members by name, and the caller
//! hands members over in whatever order it holds them. So while any object is
//! open, canonical bytes are appended to one shared buffer in the order they
//! are produced, and each open object records where each of its members lies
//! in that buffer (8 bytes of offsets per member). Closing an object sorts
//! its member records by name, read straight out of the buffer, and moves no
//! bytes: a closed object nested in another stays where it was written,
//! remembered by its sorted member list. When the outermost open object
//! closes, its text is written to the sink in canonical order by one walk
//! over the buffer with an explicit task stack, and the buffer is cleared.
//!
//! So every close costs the sort of that object's own members, and every
//! byte is copied into the buffer once and out of it once, whatever the
//! nesting. A top-level array streams element by element, but a top-level
//! *object* is held whole until it closes, and the sink sees its first byte
//! only then.
//!
//! # Bounds
//!
//! Every byte of canonical text is counted once, when it is produced, against
//! [`Limits::max_bytes`]. The buffer only ever holds produced bytes, so its
//! length never exceeds the ceiling.
//!
//! Every heap stack the writer grows is bounded by that ceiling as well: the
//! stack of open containers gains one entry per `[` or `{`, which is produced
//! first; each member record, closed-object record and walk task stands for
//! at least one produced byte. There is no depth limit: depth costs bytes,
//! and bytes are bounded.
//!
//! What the ceiling does not count:
//!
//! * 8 bytes of offsets per buffered member;
//! * 2 bytes of stack entry per open container, and a 20-byte frame per open
//!   object;
//! * a 36-byte record (with its member and child entries) per object closed
//!   inside another, until the outermost object is written out;
//! * the walk's task stack while the outermost object is written out;
//! * `Vec` growth slack (amortized doubling, up to the length again).
//!
//! So the peak heap depends on the shape. `tests/memory.rs` measures it:
//!
//! * a flat object of small members, or an object of such objects: under 4x
//!   the canonical length;
//! * a deep chain of objects with one-letter names (`{"k":{"k":...}}`), the
//!   worst shape per byte, since each 6 bytes of text carries a stack entry,
//!   a frame and a closed record: under 32x the canonical length.
//!
//! Memory is linear in the canonical length for every shape. Every structure
//! grows with `try_reserve`, so exhausting memory is a refusal, not an abort.

use alloc::string::String;
use alloc::vec::Vec;
use core::fmt;

use crate::escape::escape_fragment;
use crate::number::{exact_integer_double, with_double_text};
use crate::order::{cmp_member_names, Unescaped};
use crate::sink::try_extend;
use crate::{Error, FixedShape, LimitExceeded, LimitKind, Limits, ProtocolViolation, Sink};

/// Where one member's canonical bytes (`"name":value`) were written in the
/// shared buffer. Objects closed inside the member stay in place, so the
/// range holds their bytes in written order; the walk that writes the text
/// out substitutes their sorted form.
#[derive(Clone, Copy)]
struct Span {
    start: u32,
    end: u32,
}

/// An open object. Its finished members and closed children sit on the
/// writer's shared `open_members` and `open_children` stacks, from the
/// recorded starts up: an inner object's entries are always above its
/// parent's, and leave before the parent adds its next one.
struct Frame {
    /// Where its members start in the buffer, just after its `{`.
    start: u32,
    /// Where its finished members start on `Writer::open_members`, in the
    /// order they were written.
    members: u32,
    /// Where its children start on `Writer::open_children`: objects closed
    /// while this was the innermost open object, in written order, as
    /// indexes into `Writer::closed`.
    children: u32,
    /// Where the member being written starts, once its name is written and
    /// until its value is.
    pending: Option<u32>,
}

/// An object closed inside another, waiting to be written out.
#[derive(Clone, Copy)]
struct Closed {
    /// Its members' bytes in the buffer, from just after `{`.
    start: u32,
    end: u32,
    /// Its members, sorted, in `Writer::closed_members`.
    members: Span,
    /// Its own closed children, in written order, in `Writer::closed_children`.
    children: Span,
}

/// One open container on the writer's stack.
#[derive(Clone, Copy)]
enum Open {
    /// An array; `first` until its first element starts.
    Array { first: bool },
    /// An object; its members are buffered in the innermost [`Frame`].
    Object,
    /// serde's one-member `{"variant":value}` wrapper for an externally
    /// tagged enum. It has one member, so nothing needs sorting and it
    /// streams; `filled` once its value starts.
    Wrapper { filled: bool },
}

/// Whose members a walk task refers to.
#[derive(Clone, Copy)]
enum Owner {
    /// The outermost object being written out.
    Root,
    /// An object closed inside another, by index into `Writer::closed`.
    Closed(u32),
}

/// One step of writing the outermost object out.
enum Task {
    /// Raw buffered bytes.
    Bytes(u32, u32),
    /// `owner`'s member `next` onwards, then its closing brace.
    Body { owner: Owner, next: u32 },
    /// One member of `owner`, at `span` in the buffer.
    Member { owner: Owner, span: Span },
}

/// Writes one RFC 8785 value into a [`Sink`] from pushed events.
///
/// Push exactly one complete value, then call [`Writer::finish`]. Inside an
/// object, push [`Writer::name`] before each member's value; the writer sorts
/// the members. A value is a scalar event, or a `begin_*` event, the
/// container's contents and the matching `end_*` event.
///
/// An event out of order is refused with [`Error::Protocol`]. After any
/// refusal the writer refuses every further event with
/// [`ProtocolViolation::AfterRefusal`], and whatever the sink holds is not
/// canonical output.
///
/// ```
/// use quire_canonical::{Limits, Writer};
///
/// let mut bytes = Vec::new();
/// let mut writer = Writer::new(&mut bytes, Limits::new(64));
/// writer.begin_object()?;
/// writer.name("b")?;
/// writer.begin_array()?;
/// writer.integer(1)?;
/// writer.null()?;
/// writer.end_array()?;
/// writer.name("a")?;
/// writer.string("ö")?;
/// writer.end_object()?;
/// assert_eq!(writer.finish()?, 23);
/// assert_eq!(bytes, "{\"a\":\"ö\",\"b\":[1,null]}".as_bytes());
/// # Ok::<(), quire_canonical::Error>(())
/// ```
pub struct Writer<'s, S: Sink + ?Sized> {
    sink: &'s mut S,
    limits: Limits,
    produced: u64,
    /// Every open container, innermost last.
    stack: Vec<Open>,
    /// One frame per [`Open::Object`] on `stack`, in the same order.
    frames: Vec<Frame>,
    /// Canonical bytes produced while any object is open.
    buffer: Vec<u8>,
    /// The finished members of every open object (see [`Frame`]).
    open_members: Vec<Span>,
    /// The closed children of every open object (see [`Frame`]).
    open_children: Vec<u32>,
    /// Objects closed inside another, until the outermost one is written out.
    closed: Vec<Closed>,
    closed_members: Vec<Span>,
    closed_children: Vec<u32>,
    /// Whether the top-level value has started.
    started: bool,
    /// Whether an event has been refused.
    refused: bool,
}

impl<S: Sink + ?Sized> fmt::Debug for Writer<'_, S> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Writer")
            .field("limits", &self.limits)
            .field("produced", &self.produced)
            .field("open", &self.stack.len())
            .field("started", &self.started)
            .field("refused", &self.refused)
            .finish_non_exhaustive()
    }
}

impl<'s, S: Sink + ?Sized> Writer<'s, S> {
    /// A writer that feeds `sink` under `limits`.
    pub fn new(sink: &'s mut S, limits: Limits) -> Self {
        Self {
            sink,
            limits,
            produced: 0,
            stack: Vec::new(),
            frames: Vec::new(),
            buffer: Vec::new(),
            open_members: Vec::new(),
            open_children: Vec::new(),
            closed: Vec::new(),
            closed_members: Vec::new(),
            closed_children: Vec::new(),
            started: false,
            refused: false,
        }
    }

    /// The number of canonical bytes produced so far, including bytes still
    /// buffered in open objects.
    #[must_use]
    pub fn produced(&self) -> u64 {
        self.produced
    }

    /// Check that exactly one complete value was written, and return its
    /// canonical length.
    ///
    /// # Errors
    ///
    /// [`ProtocolViolation::Incomplete`] when no value was written or a
    /// container is still open; [`ProtocolViolation::AfterRefusal`] after an
    /// earlier refusal.
    pub fn finish(self) -> Result<u64, Error> {
        if self.refused {
            return Err(Error::Protocol(ProtocolViolation::AfterRefusal));
        }
        if !self.started || !self.stack.is_empty() {
            return Err(Error::Protocol(ProtocolViolation::Incomplete));
        }
        Ok(self.produced)
    }

    /// Write `null`.
    ///
    /// # Errors
    ///
    /// As every event: [`Error::Limit`] at the byte ceiling,
    /// [`Error::Protocol`] out of order, or the sink's refusal.
    pub fn null(&mut self) -> Result<(), Error> {
        self.guard(|writer| writer.scalar(|writer| writer.produce(b"null")))
    }

    /// Write `true` or `false`.
    ///
    /// # Errors
    ///
    /// As [`Writer::null`].
    pub fn bool(&mut self, value: bool) -> Result<(), Error> {
        let text: &[u8] = if value { b"true" } else { b"false" };
        self.guard(|writer| writer.scalar(|writer| writer.produce(text)))
    }

    /// Write a number as its RFC 8785 (ECMAScript) text.
    ///
    /// # Errors
    ///
    /// [`Error::NonFiniteNumber`] for NaN or an infinity; otherwise as
    /// [`Writer::null`].
    pub fn number(&mut self, value: f64) -> Result<(), Error> {
        self.guard(|writer| writer.scalar(|writer| writer.double(value)))
    }

    /// Write an integer by its IEEE 754 double value.
    ///
    /// # Errors
    ///
    /// [`Error::IntegerMagnitudeAboveMaximum`] when its magnitude exceeds
    /// `2^53`; otherwise as [`Writer::null`].
    pub fn integer(&mut self, value: i128) -> Result<(), Error> {
        self.guard(|writer| writer.scalar(|writer| writer.signed(value)))
    }

    /// Write a string, escaped as RFC 8785 §3.2.2.2 requires.
    ///
    /// # Errors
    ///
    /// As [`Writer::null`].
    pub fn string(&mut self, value: &str) -> Result<(), Error> {
        self.guard(|writer| writer.scalar(|writer| writer.quoted(value)))
    }

    /// Open an array.
    ///
    /// # Errors
    ///
    /// As [`Writer::null`].
    pub fn begin_array(&mut self) -> Result<(), Error> {
        self.guard(|writer| {
            writer.before_value()?;
            writer.produce(b"[")?;
            writer.push_open(Open::Array { first: true })
        })
    }

    /// Close the innermost container, which must be an array.
    ///
    /// # Errors
    ///
    /// [`ProtocolViolation::MismatchedEnd`] when it is not; otherwise as
    /// [`Writer::null`].
    pub fn end_array(&mut self) -> Result<(), Error> {
        self.guard(|writer| {
            match writer.stack.last() {
                Some(Open::Array { .. }) => {}
                _ => return Err(Error::Protocol(ProtocolViolation::MismatchedEnd)),
            }
            writer.produce(b"]")?;
            writer.stack.pop();
            writer.after_value()
        })
    }

    /// Open an object. Its members may be pushed in any order.
    ///
    /// # Errors
    ///
    /// As [`Writer::null`].
    pub fn begin_object(&mut self) -> Result<(), Error> {
        self.guard(|writer| {
            writer.before_value()?;
            writer.produce(b"{")?;
            let start = buffer_offset(&writer.buffer)?;
            reserve(&mut writer.frames, 1)?;
            writer.push_open(Open::Object)?;
            let frame = Frame {
                start,
                members: index_u32(writer.open_members.len())?,
                children: index_u32(writer.open_children.len())?,
                pending: None,
            };
            writer.frames.push(frame);
            Ok(())
        })
    }

    /// Start a member of the innermost object, which must be awaiting a
    /// name. Its value is the next value pushed.
    ///
    /// # Errors
    ///
    /// [`ProtocolViolation::NameOutsideObject`] outside an object,
    /// [`ProtocolViolation::NameWithoutValue`] when the previous name has no
    /// value yet; otherwise as [`Writer::null`].
    pub fn name(&mut self, name: &str) -> Result<(), Error> {
        self.guard(|writer| writer.begin_member(name))
    }

    /// Close the innermost container, which must be an object: sort its
    /// members and move them to their destination.
    ///
    /// # Errors
    ///
    /// [`Error::DuplicateMemberName`] when two members share a name,
    /// [`ProtocolViolation::MismatchedEnd`] when the innermost container is
    /// not an object, [`ProtocolViolation::ObjectEndedAfterName`] when the
    /// last name has no value; otherwise as [`Writer::null`].
    pub fn end_object(&mut self) -> Result<(), Error> {
        self.guard(Self::close_object)
    }

    /// Write `value`, whose depth is fixed by its type, through its serde
    /// encoding.
    ///
    /// # Errors
    ///
    /// The refusals of [`crate::encode`] for `value`, or as
    /// [`Writer::null`].
    pub fn serialize<T: FixedShape + ?Sized>(&mut self, value: &T) -> Result<(), Error> {
        self.guard(|writer| crate::encoder::serialize(writer, value))
    }

    /// Run `event`, and remember a refusal so no later event is accepted.
    fn guard(&mut self, event: impl FnOnce(&mut Self) -> Result<(), Error>) -> Result<(), Error> {
        if self.refused {
            return Err(Error::Protocol(ProtocolViolation::AfterRefusal));
        }
        let outcome = event(self);
        if outcome.is_err() {
            self.refused = true;
        }
        outcome
    }

    /// Write one scalar value with `write`.
    pub(crate) fn scalar(
        &mut self,
        write: impl FnOnce(&mut Self) -> Result<(), Error>,
    ) -> Result<(), Error> {
        self.before_value()?;
        write(self)?;
        self.after_value()
    }

    /// Prepare the current position for the start of a value.
    fn before_value(&mut self) -> Result<(), Error> {
        let Some(open) = self.stack.last_mut() else {
            if self.started {
                return Err(Error::Protocol(ProtocolViolation::SecondValue));
            }
            self.started = true;
            return Ok(());
        };
        match open {
            Open::Array { first } => {
                let separator = !*first;
                *first = false;
                if separator {
                    self.produce(b",")?;
                }
                Ok(())
            }
            Open::Object => {
                if self.top_frame()?.pending.is_none() {
                    return Err(Error::Protocol(ProtocolViolation::ValueWithoutName));
                }
                Ok(())
            }
            Open::Wrapper { filled } => {
                if *filled {
                    return Err(Error::Internal {
                        invariant: "an enum wrapper holds exactly one value",
                    });
                }
                *filled = true;
                Ok(())
            }
        }
    }

    /// A value has just been completed at the current position.
    fn after_value(&mut self) -> Result<(), Error> {
        match self.stack.last() {
            Some(Open::Object) => self.end_member(),
            Some(Open::Array { .. } | Open::Wrapper { .. }) | None => Ok(()),
        }
    }

    fn push_open(&mut self, open: Open) -> Result<(), Error> {
        reserve(&mut self.stack, 1)?;
        self.stack.push(open);
        Ok(())
    }

    /// Count `length` new canonical bytes against the ceiling.
    fn count(&mut self, length: usize) -> Result<(), Error> {
        let bound = self.limits.max_bytes();
        let required = u64::try_from(length)
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
        self.produced = required;
        Ok(())
    }

    /// Count new canonical bytes against the ceiling, then send them to the
    /// sink, or to the buffer while an object is open.
    pub(crate) fn produce(&mut self, bytes: &[u8]) -> Result<(), Error> {
        self.count(bytes.len())?;
        if self.frames.is_empty() {
            self.sink.write_bytes(bytes)
        } else {
            try_extend(&mut self.buffer, bytes)
        }
    }

    pub(crate) fn quoted(&mut self, text: &str) -> Result<(), Error> {
        self.produce(b"\"")?;
        escape_fragment(text, |run| self.produce(run))?;
        self.produce(b"\"")
    }

    pub(crate) fn double(&mut self, value: f64) -> Result<(), Error> {
        with_double_text(value, |text| self.produce(text))
    }

    pub(crate) fn signed(&mut self, value: i128) -> Result<(), Error> {
        let double = exact_integer_double(value < 0, value.unsigned_abs())
            .ok_or(Error::IntegerMagnitudeAboveMaximum(value))?;
        self.double(double)
    }

    pub(crate) fn unsigned(&mut self, value: u128) -> Result<(), Error> {
        let double = exact_integer_double(false, value)
            .ok_or(Error::UnsignedIntegerMagnitudeAboveMaximum(value))?;
        self.double(double)
    }

    fn top_frame(&mut self) -> Result<&mut Frame, Error> {
        self.frames.last_mut().ok_or(Error::Internal {
            invariant: "every open object has a frame",
        })
    }

    /// Write `"name":` as the start of a new member of the innermost object.
    pub(crate) fn begin_member(&mut self, name: &str) -> Result<(), Error> {
        if !matches!(self.stack.last(), Some(Open::Object)) {
            return Err(Error::Protocol(ProtocolViolation::NameOutsideObject));
        }
        let offset = buffer_offset(&self.buffer)?;
        let frame = self.top_frame()?;
        if frame.pending.is_some() {
            return Err(Error::Protocol(ProtocolViolation::NameWithoutValue));
        }
        frame.pending = Some(offset);
        self.quoted(name)?;
        self.produce(b":")
    }

    /// The member's value has been written; record the finished member.
    fn end_member(&mut self) -> Result<(), Error> {
        let end = buffer_offset(&self.buffer)?;
        let frame = self.top_frame()?;
        let start = frame.pending.take().ok_or(Error::Internal {
            invariant: "a member value follows its name",
        })?;
        reserve(&mut self.open_members, 1)?;
        self.open_members.push(Span { start, end });
        Ok(())
    }

    pub(crate) fn close_object(&mut self) -> Result<(), Error> {
        if !matches!(self.stack.last(), Some(Open::Object)) {
            return Err(Error::Protocol(ProtocolViolation::MismatchedEnd));
        }
        if self.top_frame()?.pending.is_some() {
            return Err(Error::Protocol(ProtocolViolation::ObjectEndedAfterName));
        }
        let frame = self.frames.pop().ok_or(Error::Internal {
            invariant: "every open object has a frame",
        })?;
        self.stack.pop();
        let buffer = &self.buffer;
        let members =
            self.open_members
                .get_mut(frame.members as usize..)
                .ok_or(Error::Internal {
                    invariant: "an open object's members start inside the stack",
                })?;
        // Unstable sort: in place, no allocation. Names are unique (checked
        // next), so stability cannot matter. A span outside the buffer sorts
        // as empty here and is refused below.
        members.sort_unstable_by(|left, right| {
            cmp_member_names(
                span_bytes(buffer, *left).unwrap_or_default(),
                span_bytes(buffer, *right).unwrap_or_default(),
            )
        });
        for pair in members.windows(2) {
            let [left, right] = pair else { continue };
            let member = span_bytes(buffer, *left)?;
            if cmp_member_names(member, span_bytes(buffer, *right)?).is_eq() {
                let name: Vec<u8> = Unescaped::member_name(member).collect();
                return Err(Error::DuplicateMemberName {
                    name: String::from_utf8_lossy(&name).into_owned(),
                });
            }
        }
        // The commas between members and the closing brace are counted now
        // and written by the walk.
        let member_count = members.len();
        self.count(member_count.max(1))?;
        if self.frames.is_empty() {
            // The outermost object: its entries are the whole of both stacks.
            let members = core::mem::take(&mut self.open_members);
            let children = core::mem::take(&mut self.open_children);
            self.write_out(&members, &children)?;
        } else {
            let members = slice_of(
                &self.open_members,
                Span {
                    start: frame.members,
                    end: index_u32(self.open_members.len())?,
                },
            )?;
            let children = slice_of(
                &self.open_children,
                Span {
                    start: frame.children,
                    end: index_u32(self.open_children.len())?,
                },
            )?;
            let closed = Closed {
                start: frame.start,
                end: buffer_offset(&self.buffer)?,
                members: append(&mut self.closed_members, members)?,
                children: append(&mut self.closed_children, children)?,
            };
            self.open_members.truncate(frame.members as usize);
            self.open_children.truncate(frame.children as usize);
            let id = index_u32(self.closed.len())?;
            reserve(&mut self.closed, 1)?;
            self.closed.push(closed);
            reserve(&mut self.open_children, 1)?;
            self.open_children.push(id);
        }
        self.after_value()
    }

    /// Write the outermost object, just closed with `members` (sorted) and
    /// `children`, to the sink in canonical order, then clear the buffer. One
    /// walk with an explicit task stack: each object's members in sorted
    /// order, each member's bytes with the objects closed inside it replaced
    /// by their own sorted text. The stack holds one entry per open level of
    /// the walk and one per child object of the member being written.
    fn write_out(&mut self, members: &[Span], children: &[u32]) -> Result<(), Error> {
        let mut tasks = Vec::new();
        reserve(&mut tasks, 1)?;
        tasks.push(Task::Body {
            owner: Owner::Root,
            next: 0,
        });
        while let Some(task) = tasks.pop() {
            match task {
                Task::Bytes(start, end) => {
                    if start < end {
                        let bytes = span_bytes(&self.buffer, Span { start, end })?;
                        self.sink.write_bytes(bytes)?;
                    }
                }
                Task::Body { owner, next } => {
                    let (own_members, _) = self.lists(owner, members, children)?;
                    match own_members.get(next as usize).copied() {
                        None => self.sink.write_bytes(b"}")?,
                        Some(span) => {
                            if next > 0 {
                                self.sink.write_bytes(b",")?;
                            }
                            reserve(&mut tasks, 2)?;
                            tasks.push(Task::Body {
                                owner,
                                next: next.saturating_add(1),
                            });
                            tasks.push(Task::Member { owner, span });
                        }
                    }
                }
                Task::Member { owner, span } => {
                    let (_, own_children) = self.lists(owner, members, children)?;
                    // Children are in written order, so those inside this
                    // member are one contiguous run. A child starts after
                    // its member's name, and an empty child ends exactly
                    // where its member does.
                    let starts_by = |offset: u32| {
                        own_children.partition_point(|child| {
                            self.closed
                                .get(*child as usize)
                                .is_some_and(|nested| nested.start <= offset)
                        })
                    };
                    let first = starts_by(span.start);
                    let last = starts_by(span.end);
                    let inside = own_children.get(first..last).unwrap_or_default();
                    reserve(&mut tasks, inside.len().saturating_mul(2).saturating_add(1))?;
                    let mut cursor = span.end;
                    for child in inside.iter().rev() {
                        let nested = self.closed_at(*child)?;
                        tasks.push(Task::Bytes(nested.end, cursor));
                        tasks.push(Task::Body {
                            owner: Owner::Closed(*child),
                            next: 0,
                        });
                        cursor = nested.start;
                    }
                    tasks.push(Task::Bytes(span.start, cursor));
                }
            }
        }
        self.buffer.clear();
        self.closed.clear();
        self.closed_members.clear();
        self.closed_children.clear();
        Ok(())
    }

    /// The sorted members and the children of `owner`.
    fn lists<'a>(
        &'a self,
        owner: Owner,
        root_members: &'a [Span],
        root_children: &'a [u32],
    ) -> Result<(&'a [Span], &'a [u32]), Error> {
        match owner {
            Owner::Root => Ok((root_members, root_children)),
            Owner::Closed(id) => {
                let closed = self.closed_at(id)?;
                Ok((
                    slice_of(&self.closed_members, closed.members)?,
                    slice_of(&self.closed_children, closed.children)?,
                ))
            }
        }
    }

    fn closed_at(&self, id: u32) -> Result<Closed, Error> {
        self.closed
            .get(id as usize)
            .copied()
            .ok_or(Error::Internal {
                invariant: "closed-object ids index the closed list",
            })
    }

    /// `{"variant":` — the wrapper serde's externally tagged enums use.
    pub(crate) fn begin_wrapper(&mut self, variant: &str) -> Result<(), Error> {
        self.before_value()?;
        self.produce(b"{")?;
        self.push_open(Open::Wrapper { filled: false })?;
        self.quoted(variant)?;
        self.produce(b":")
    }

    pub(crate) fn end_wrapper(&mut self) -> Result<(), Error> {
        match self.stack.last() {
            Some(Open::Wrapper { filled: true }) => {}
            _ => {
                return Err(Error::Internal {
                    invariant: "an enum wrapper closes after its one value",
                })
            }
        }
        self.produce(b"}")?;
        self.stack.pop();
        self.after_value()
    }
}

/// The current end of the buffer as a 32-bit offset.
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

/// A list index as a 32-bit record field. Each record stands for at least
/// one buffered byte, so it fits whenever the buffer's offsets do.
fn index_u32(index: usize) -> Result<u32, Error> {
    u32::try_from(index).map_err(|_| Error::Internal {
        invariant: "record indexes fit u32 while buffer offsets do",
    })
}

fn span_bytes(buffer: &[u8], span: Span) -> Result<&[u8], Error> {
    buffer
        .get(span.start as usize..span.end as usize)
        .ok_or(Error::Internal {
            invariant: "member spans lie inside the buffer",
        })
}

fn slice_of<T>(list: &[T], span: Span) -> Result<&[T], Error> {
    list.get(span.start as usize..span.end as usize)
        .ok_or(Error::Internal {
            invariant: "record ranges lie inside their list",
        })
}

/// Append `items` to `list`, returning where they landed.
fn append<T: Copy>(list: &mut Vec<T>, items: &[T]) -> Result<Span, Error> {
    let start = index_u32(list.len())?;
    reserve(list, items.len())?;
    list.extend_from_slice(items);
    Ok(Span {
        start,
        end: index_u32(list.len())?,
    })
}

fn reserve<T>(list: &mut Vec<T>, additional: usize) -> Result<(), Error> {
    list.try_reserve(additional).map_err(|_| Error::Allocation {
        requested: core::mem::size_of::<T>().saturating_mul(additional),
    })
}
