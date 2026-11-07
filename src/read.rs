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
//! of its lists: the nodes, the child ids and object members, the decoded
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
//! A number keeps its source text next to its double value: RFC 8785 encodes
//! the double, while a typed reader may need an integer exactly as written.

use alloc::string::String;
use alloc::vec::Vec;
use core::fmt;

use crate::reservation::{Provision, Refusal};
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
    /// The heap bytes this document holds: the capacity of its node, child,
    /// member and text lists, the unit the reader's reservation is asked in.
    #[must_use]
    pub fn retained_bytes(&self) -> usize {
        fn bytes<T>(list: &Vec<T>) -> usize {
            list.capacity().saturating_mul(core::mem::size_of::<T>())
        }
        self.text
            .capacity()
            .saturating_add(bytes(&self.slots))
            .saturating_add(bytes(&self.items))
            .saturating_add(bytes(&self.members))
    }

    /// The value at `id`, or `None` when `id` is past this document's last
    /// value, as an id from a larger document is. An id from another document
    /// of at least this size names some value here, not the one it named
    /// there. Does not allocate.
    #[must_use]
    pub fn node(&self, id: NodeId) -> Option<NodeRef<'_>> {
        (id.0 < self.slots.len()).then_some(NodeRef {
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
        self.slots.get(id).copied().unwrap_or(Slot::Null)
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
        writer.reserve_list(&mut tasks, 1)?;
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
                        writer.reserve_list(&mut tasks, ids.len().saturating_add(1))?;
                        tasks.push(Task::EndArray);
                        tasks.extend(ids.iter().rev().map(|id| Task::Value(*id)));
                    }
                    Slot::Object(range) => {
                        writer.begin_object()?;
                        let pairs = document
                            .members
                            .get(range.start..range.end)
                            .unwrap_or_default();
                        writer.reserve_list(
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

fn read_provisioned(
    input: &[u8],
    max_input_bytes: u64,
    provision: Provision<'_>,
) -> Result<Document, ReadError> {
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
    Parser::new(source, provision).run()
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

struct Parser<'i, 'r> {
    provision: Provision<'r>,
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

impl<'i, 'r> Parser<'i, 'r> {
    fn new(source: &'i str, provision: Provision<'r>) -> Self {
        Self {
            provision,
            source,
            input: source.as_bytes(),
            position: 0,
            text: String::new(),
            slots: Vec::new(),
            items: Vec::new(),
            members: Vec::new(),
            stack: Vec::new(),
            scratch_items: Vec::new(),
            scratch_members: Vec::new(),
            order: Vec::new(),
        }
    }

    /// Append `piece` to the decoded text, asking the reservation first.
    fn append(&mut self, piece: &str) -> Result<(), ReadError> {
        self.provision
            .grow_text(&mut self.text, piece.len())
            .map_err(ReadError::from)?;
        self.text.push_str(piece);
        Ok(())
    }

    fn malformed(&self, offset: usize, kind: Malformed) -> ReadError {
        ReadError::Malformed { offset, kind }
    }

    /// The refusal for the number `literal` at `offset`, with its JSON pointer.
    fn number_out_of_range(&mut self, offset: usize, literal: &str) -> ReadError {
        match self
            .pointer(None)
            .and_then(|pointer| Ok((pointer, self.owned(literal)?)))
        {
            Ok((pointer, lexeme)) => ReadError::NumberOutOfRange {
                offset,
                pointer,
                lexeme,
            },
            Err(error) => error,
        }
    }

    /// An owned copy of `text`, charged to the reservation and reserved
    /// fallibly.
    fn owned(&mut self, text: &str) -> Result<String, ReadError> {
        let mut copy = String::new();
        self.provision
            .grow_text(&mut copy, text.len())
            .map_err(ReadError::from)?;
        copy.push_str(text);
        Ok(copy)
    }

    /// The RFC 6901 pointer of the value being read, from the open containers,
    /// below the member `name` of the object whose members start at
    /// `children_start` on `scratch_members` when given.
    fn pointer(&mut self, below: Option<(Range, usize)>) -> Result<String, ReadError> {
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
            self.provision
                .grow(&mut steps, 1)
                .map_err(ReadError::from)?;
            steps.push(Step::Name(name));
            // The object being closed still has its members on the shared
            // list; the ones below it belong to the objects around it.
            members_end = children_start;
        }
        for open in self.stack.iter().rev() {
            if open.object {
                let member = members_end
                    .checked_sub(1)
                    .and_then(|index| self.scratch_members.get(index));
                if let Some(member) = member {
                    self.provision
                        .grow(&mut steps, 1)
                        .map_err(ReadError::from)?;
                    steps.push(Step::Name(member.name));
                }
                members_end = open.start;
            } else {
                self.provision
                    .grow(&mut steps, 1)
                    .map_err(ReadError::from)?;
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
        self.provision
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
                self.provision
                    .grow(&mut self.stack, 1)
                    .map_err(ReadError::from)?;
                self.stack.push(OpenContainer {
                    object,
                    start: if object {
                        self.scratch_members.len()
                    } else {
                        self.scratch_items.len()
                    },
                });
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
        self.provision
            .grow(&mut self.slots, 1)
            .map_err(ReadError::from)?;
        self.slots.push(slot);
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
            Some(OpenContainer { object: false, .. }) => {
                self.provision
                    .grow(&mut self.scratch_items, 1)
                    .map_err(ReadError::from)?;
                self.scratch_items.push(id);
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
        self.provision
            .grow(&mut self.scratch_members, 1)
            .map_err(ReadError::from)?;
        self.scratch_members.push(PendingMember {
            name,
            offset,
            id: 0,
        });
        Ok(())
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
            self.provision
                .grow(&mut self.members, pending.len())
                .map_err(ReadError::from)?;
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
            self.provision
                .grow(&mut self.items, items.len())
                .map_err(ReadError::from)?;
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
        self.provision
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
        let pointer = self.pointer(Some((name, start)))?;
        Err(ReadError::DuplicateName { offset, pointer })
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
            self.append(run)?;
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
            return Err(self.number_out_of_range(start, literal));
        }
        let text_start = self.text.len();
        self.append(literal)?;
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
