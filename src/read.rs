// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Agent-IX
//! The shared reader of untrusted JSON, its arena tree, and the tree's
//! canonical encoder.
//!
//! [`read`] parses RFC 8259 JSON text into a [`Document`] with one loop over
//! the input and an explicit heap stack of open containers; nothing recurses,
//! so a document of any depth reads on any thread stack. The tree is an
//! arena: flat vectors of nodes, child ids and decoded text. Its `Clone`,
//! `Debug`, `PartialEq` and `Drop` therefore walk flat vectors and never
//! recurse either.
//!
//! # Bounds
//!
//! The one limit is the caller's input byte limit, checked before any work.
//! Every heap structure the reader grows is linear in the input bytes: each
//! node, each open container on the stack and each child id is introduced by
//! at least one input byte, and decoded text is never longer than its
//! escaped source. So the input byte limit bounds memory as well as time.
//! There is no depth limit. Growth uses `try_reserve`, so exhausting memory
//! is a refusal, not an abort.
//!
//! The constant is not small: a node costs a 32-byte slot and an 8- or
//! 24-byte child entry, an open container a 16-byte stack entry, and an open
//! object's member a 32-byte pending record, before `Vec` growth slack. For
//! inputs made of one- and two-byte values (`[[[...]]]`, `[1,1,...]`,
//! `{"k":{"k":...}}`) `tests/memory.rs` measures the peak heap at under 64x
//! the input length (between 19x and 47x). Size the input byte limit with
//! that multiple in mind: a 16 MiB input may take up to 1 GiB while it is
//! read.
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
//! A number keeps its source text next to its double value: RFC 8785 encodes
//! the double, while a typed reader may need an integer exactly as written.

use alloc::string::String;
use alloc::vec::Vec;
use core::fmt;

use crate::{Encode, Error, LimitExceeded, LimitKind, Sink, Writer};

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
    /// A heap reservation for the tree failed.
    #[error("allocation of {requested} bytes for the JSON tree failed")]
    Allocation {
        /// The size of the reservation that failed.
        requested: usize,
    },
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
    /// A member name already used earlier in the same object.
    DuplicateName,
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
            Self::DuplicateName => "duplicate member name",
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

/// One node of the arena.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Slot {
    Null,
    Bool(bool),
    /// The literal's source text in `text`, and its double.
    Number {
        text: Range,
        value: f64,
    },
    /// The decoded string in `text`.
    String(Range),
    /// Child ids in `items`.
    Array(Range),
    /// `(name, value)` pairs in `members`, in document order.
    Object(Range),
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
#[derive(Clone, Debug, PartialEq)]
pub struct Document {
    /// Decoded strings and number literals, back to back.
    text: String,
    slots: Vec<Slot>,
    items: Vec<usize>,
    members: Vec<(Range, usize)>,
    root: usize,
}

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
    ids: core::slice::Iter<'d, usize>,
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
        let id = *self.ids.next()?;
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
    pairs: core::slice::Iter<'d, (Range, usize)>,
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
        let (name, id) = *self.pairs.next()?;
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
        self.slots.get(id).copied().unwrap_or(Slot::Null)
    }
}

impl<'d> NodeRef<'d> {
    /// What this value holds.
    #[must_use]
    pub fn node(&self) -> Node<'d> {
        let document = self.document;
        match document.slot(self.id) {
            Slot::Null => Node::Null,
            Slot::Bool(value) => Node::Bool(value),
            Slot::Number { text, value } => Node::Number(Number {
                text: document.text_of(text),
                value,
            }),
            Slot::String(text) => Node::String(document.text_of(text)),
            Slot::Array(range) => Node::Array(Items {
                document,
                ids: document
                    .items
                    .get(range.start..range.end)
                    .unwrap_or_default()
                    .iter(),
            }),
            Slot::Object(range) => Node::Object(Members {
                document,
                pairs: document
                    .members
                    .get(range.start..range.end)
                    .unwrap_or_default()
                    .iter(),
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
        reserve_tasks(writer, &mut tasks, 1)?;
        tasks.push(Task::Value(self.id));
        while let Some(task) = tasks.pop() {
            match task {
                Task::Value(id) => match document.slot(id) {
                    Slot::Null => writer.null()?,
                    Slot::Bool(value) => writer.bool(value)?,
                    Slot::Number { value, .. } => writer.number(value)?,
                    Slot::String(text) => writer.string(document.text_of(text))?,
                    Slot::Array(range) => {
                        writer.begin_array()?;
                        let ids = document
                            .items
                            .get(range.start..range.end)
                            .unwrap_or_default();
                        reserve_tasks(writer, &mut tasks, ids.len().saturating_add(1))?;
                        tasks.push(Task::EndArray);
                        tasks.extend(ids.iter().rev().map(|id| Task::Value(*id)));
                    }
                    Slot::Object(range) => {
                        writer.begin_object()?;
                        let pairs = document
                            .members
                            .get(range.start..range.end)
                            .unwrap_or_default();
                        reserve_tasks(
                            writer,
                            &mut tasks,
                            pairs.len().saturating_mul(2).saturating_add(1),
                        )?;
                        tasks.push(Task::EndObject);
                        for (name, id) in pairs.iter().rev() {
                            tasks.push(Task::Value(*id));
                            tasks.push(Task::Name(*name));
                        }
                    }
                },
                Task::Name(name) => writer.name(document.text_of(name))?,
                Task::EndArray => writer.end_array()?,
                Task::EndObject => writer.end_object()?,
            }
        }
        return Ok(());

        /// Reserve room for `additional` tasks, or refuse through `writer`
        /// so it accepts no later event.
        fn reserve_tasks<T, S: Sink + ?Sized>(
            writer: &mut Writer<'_, S>,
            tasks: &mut Vec<T>,
            additional: usize,
        ) -> Result<(), Error> {
            if tasks.try_reserve(additional).is_err() {
                return writer.refuse(Error::Allocation {
                    requested: core::mem::size_of::<T>().saturating_mul(additional),
                });
            }
            Ok(())
        }
    }
}

/// Read `input` as one JSON value into a [`Document`].
///
/// # Errors
///
/// [`ReadError::Limit`] when `input` is longer than `max_input_bytes`,
/// before any of it is read; [`ReadError::Malformed`] with the byte offset of
/// the fault otherwise (see the module docs for what is refused);
/// [`ReadError::Allocation`] when memory runs out.
pub fn read(input: &[u8], max_input_bytes: u64) -> Result<Document, ReadError> {
    let length = u64::try_from(input.len()).unwrap_or(u64::MAX);
    if length > max_input_bytes {
        return Err(LimitExceeded {
            kind: LimitKind::InputBytes,
            bound: max_input_bytes,
            required: length,
        }
        .into());
    }
    let source = core::str::from_utf8(input).map_err(|error| ReadError::Malformed {
        offset: error.valid_up_to(),
        kind: Malformed::InvalidUtf8,
    })?;
    Parser::new(source)?.run()
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
struct OpenContainer {
    object: bool,
    /// Where this container's children start on the parser's
    /// `scratch_members` (an object) or `scratch_items` (an array).
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

struct Parser<'i> {
    source: &'i str,
    input: &'i [u8],
    position: usize,
    text: String,
    slots: Vec<Slot>,
    items: Vec<usize>,
    members: Vec<(Range, usize)>,
    stack: Vec<OpenContainer>,
    /// The children of every open array, innermost last.
    scratch_items: Vec<usize>,
    /// The members of every open object, innermost last.
    scratch_members: Vec<PendingMember>,
    /// Reused order buffer for the duplicate-name check.
    order: Vec<usize>,
}

fn reserve<T>(vector: &mut Vec<T>, additional: usize) -> Result<(), ReadError> {
    vector
        .try_reserve(additional)
        .map_err(|_| ReadError::Allocation {
            requested: core::mem::size_of::<T>().saturating_mul(additional),
        })
}

/// An owned copy of `text`, reserved fallibly.
fn owned(text: &str) -> Result<String, ReadError> {
    let mut copy = String::new();
    copy.try_reserve(text.len())
        .map_err(|_| ReadError::Allocation {
            requested: text.len(),
        })?;
    copy.push_str(text);
    Ok(copy)
}

fn push<T>(vector: &mut Vec<T>, value: T) -> Result<(), ReadError> {
    reserve(vector, 1)?;
    vector.push(value);
    Ok(())
}

impl<'i> Parser<'i> {
    fn new(source: &'i str) -> Result<Self, ReadError> {
        let mut text = String::new();
        // Decoded text never outgrows its source, so this one reservation
        // holds every string and number literal.
        text.try_reserve(source.len())
            .map_err(|_| ReadError::Allocation {
                requested: source.len(),
            })?;
        Ok(Self {
            source,
            input: source.as_bytes(),
            position: 0,
            text,
            slots: Vec::new(),
            items: Vec::new(),
            members: Vec::new(),
            stack: Vec::new(),
            scratch_items: Vec::new(),
            scratch_members: Vec::new(),
            order: Vec::new(),
        })
    }

    fn malformed(&self, offset: usize, kind: Malformed) -> ReadError {
        ReadError::Malformed { offset, kind }
    }

    /// The refusal for the number `literal` at `offset`, with its JSON pointer.
    fn number_out_of_range(&self, offset: usize, literal: &str) -> ReadError {
        match self
            .pointer()
            .and_then(|pointer| Ok((pointer, owned(literal)?)))
        {
            Ok((pointer, lexeme)) => ReadError::NumberOutOfRange {
                offset,
                pointer,
                lexeme,
            },
            Err(error) => error,
        }
    }

    /// The RFC 6901 pointer of the value being read, from the open containers.
    fn pointer(&self) -> Result<String, ReadError> {
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
        for open in self.stack.iter().rev() {
            if open.object {
                let member = members_end
                    .checked_sub(1)
                    .and_then(|index| self.scratch_members.get(index));
                if let Some(member) = member {
                    push(&mut steps, Step::Name(member.name))?;
                }
                members_end = open.start;
            } else {
                push(
                    &mut steps,
                    Step::Index(items_end.saturating_sub(open.start)),
                )?;
                items_end = open.start;
            }
        }
        let mut pointer = String::new();
        // Escaping only grows a name, by at most twice.
        pointer
            .try_reserve(
                self.text
                    .len()
                    .saturating_mul(2)
                    .saturating_add(steps.len() * 21),
            )
            .map_err(|_| ReadError::Allocation {
                requested: self.text.len(),
            })?;
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

    fn run(mut self) -> Result<Document, ReadError> {
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
                        break self.slots.len().checked_sub(1);
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
        let root = root.ok_or_else(|| self.malformed(self.position, Malformed::UnexpectedEnd))?;
        Ok(Document {
            text: self.text,
            slots: self.slots,
            items: self.items,
            members: self.members,
            root,
        })
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
                push(
                    &mut self.stack,
                    OpenContainer {
                        object,
                        start: if object {
                            self.scratch_members.len()
                        } else {
                            self.scratch_items.len()
                        },
                    },
                )?;
                return Ok(if object {
                    Expect::NameOrEnd
                } else {
                    Expect::ValueOrEnd
                });
            }
            b'"' => Slot::String(self.string()?),
            b't' => self.literal("true", Slot::Bool(true))?,
            b'f' => self.literal("false", Slot::Bool(false))?,
            b'n' => self.literal("null", Slot::Null)?,
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
        let id = self.slots.len();
        push(&mut self.slots, slot)?;
        Ok(id)
    }

    /// Attach a finished value to its container, or make it the root.
    fn complete(&mut self, id: usize) -> Result<Expect, ReadError> {
        match self.stack.last() {
            Some(OpenContainer { object: true, .. }) => {
                let member = self
                    .scratch_members
                    .last_mut()
                    .ok_or(ReadError::Malformed {
                        offset: self.position,
                        kind: Malformed::UnexpectedCharacter,
                    })?;
                member.id = id;
            }
            Some(OpenContainer { object: false, .. }) => push(&mut self.scratch_items, id)?,
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
        push(
            &mut self.scratch_members,
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
        let slot = if object {
            self.check_unique_names(open.start)?;
            let pending = self.scratch_members.get(open.start..).unwrap_or_default();
            let start = self.members.len();
            reserve(&mut self.members, pending.len())?;
            self.members
                .extend(pending.iter().map(|member| (member.name, member.id)));
            self.scratch_members.truncate(open.start);
            Slot::Object(Range {
                start,
                end: self.members.len(),
            })
        } else {
            let items = self.scratch_items.get(open.start..).unwrap_or_default();
            let start = self.items.len();
            reserve(&mut self.items, items.len())?;
            self.items.extend_from_slice(items);
            self.scratch_items.truncate(open.start);
            Slot::Array(Range {
                start,
                end: self.items.len(),
            })
        };
        self.node(slot)
    }

    /// Refuse the object whose members start at `start` on
    /// `scratch_members` when two share a name, at the later one's offset.
    fn check_unique_names(&mut self, start: usize) -> Result<(), ReadError> {
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
        reserve(&mut self.order, children.len())?;
        self.order.extend(0..children.len());
        self.order
            .sort_unstable_by(|left, right| name_of(*left).cmp(&name_of(*right)));
        for pair in self.order.windows(2) {
            let [left, right] = pair else { continue };
            let (left_name, _) = name_of(*left);
            let (right_name, later) = name_of(*right);
            if left_name == right_name {
                // Sorted by (name, offset), so `right` is the later one.
                return Err(ReadError::Malformed {
                    offset: later,
                    kind: Malformed::DuplicateName,
                });
            }
        }
        Ok(())
    }

    /// Read the string starting at the current `"`, decoding it into `text`.
    fn string(&mut self) -> Result<Range, ReadError> {
        self.position += 1;
        let start = self.text.len();
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
            self.text.push_str(run);
            match self.peek() {
                Some(b'"') => {
                    self.position += 1;
                    return Ok(Range {
                        start,
                        end: self.text.len(),
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
        self.text.push(decoded);
        Ok(())
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
            return Err(self.number_out_of_range(start, literal));
        }
        let text_start = self.text.len();
        self.text.push_str(literal);
        Ok(Slot::Number {
            text: Range {
                start: text_start,
                end: self.text.len(),
            },
            value,
        })
    }

    fn digits(&mut self) {
        while matches!(self.peek(), Some(b'0'..=b'9')) {
            self.position += 1;
        }
    }
}
