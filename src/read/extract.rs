// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Agent-IX
//! Copying chosen subtrees of a [`Document`] into a new, exact-size one.
//!
//! The reader builds its arena in post-order: a container's node is created
//! when it closes, after every node inside it. So the values of one subtree
//! occupy one contiguous run of ids, ending at the subtree's root and
//! starting at its leftmost leaf, which is reached by following first
//! children. Extraction relies on that layout and checks it (see
//! [`Span::of`]); it needs no stack, no recursion and no heap besides the new
//! document and the returned ids.

use alloc::string::String;
use alloc::vec::Vec;

use super::{Document, NodeId, Range, Slot, SlotKind};
use crate::reservation::{Provision, Refusal};
use crate::Reservation;

/// Why [`Document::extract_with_reservation`] returned no document.
#[derive(Clone, Debug, Eq, Hash, PartialEq, thiserror::Error)]
#[non_exhaustive]
pub enum ExtractError {
    /// The caller's [`Reservation`] refused one of the new document's lists.
    /// Nothing was allocated for it, no later list was asked for, and the
    /// lists already allocated were dropped.
    #[error("heap reservation of {requested} bytes for the extracted document was denied")]
    ReservationDenied {
        /// The bytes the refused list asked for.
        requested: usize,
    },
    /// The allocator could not provide a list the reservation admitted, or
    /// a list's size does not fit `usize`.
    #[error("allocation of {requested} bytes for the extracted document failed")]
    Allocation {
        /// The size of the allocation that failed.
        requested: usize,
    },
    /// A requested root is not a value of this document. Checked before the
    /// reservation is asked anything.
    #[error("node {0:?} is not a value of this document")]
    UnknownNode(NodeId),
}

impl From<Refusal> for ExtractError {
    fn from(refusal: Refusal) -> Self {
        match refusal {
            Refusal::Denied { requested } => Self::ReservationDenied { requested },
            Refusal::Allocation { requested } => Self::Allocation { requested },
        }
    }
}

/// The exact sizes of a set of copied subtrees.
#[derive(Clone, Copy, Default)]
struct Totals {
    text: usize,
    slots: usize,
    items: usize,
    members: usize,
}

impl Totals {
    fn add(self, other: Self) -> Option<Self> {
        Some(Self {
            text: self.text.checked_add(other.text)?,
            slots: self.slots.checked_add(other.slots)?,
            items: self.items.checked_add(other.items)?,
            members: self.members.checked_add(other.members)?,
        })
    }
}

/// One subtree: the contiguous id run `first..=root` and its sizes.
struct Span {
    first: usize,
    root: usize,
    totals: Totals,
}

impl Span {
    /// The subtree at `root`, counted without heap.
    ///
    /// Besides the sizes, the scan checks that the run is exactly the
    /// subtree: every child id of a value in the run lies in the run before
    /// its parent, and the run holds one child edge per value except the
    /// root. Each value has one parent, which comes after it; so every value
    /// in the run reaches `root` by its parents, and no value outside the run
    /// is reached from it. A document from [`crate::read`] always passes;
    /// `None` means a layout this function cannot copy.
    fn of(document: &Document, root: usize) -> Option<Self> {
        let mut first = root;
        loop {
            let child = match document.slot(first).kind(document) {
                SlotKind::Array(range) if range.start < range.end => {
                    *document.items.get(range.start)?
                }
                SlotKind::Object(range) if range.start < range.end => {
                    document.members.get(range.start)?.1
                }
                _ => break,
            };
            if child >= first {
                return None;
            }
            first = child;
        }
        let mut totals = Totals {
            slots: root - first + 1,
            ..Totals::default()
        };
        let mut edges = 0_usize;
        for id in first..=root {
            let inside = |child: usize| (first..id).contains(&child);
            match document.slot(id).kind(document) {
                SlotKind::Null | SlotKind::Bool(_) => {}
                SlotKind::Number { text, .. } | SlotKind::String(text) => {
                    totals.text = totals.text.checked_add(text.end - text.start)?;
                }
                SlotKind::Array(range) => {
                    let ids = document.items.get(range.start..range.end)?;
                    if !ids.iter().all(|child| inside(*child)) {
                        return None;
                    }
                    totals.items = totals.items.checked_add(ids.len())?;
                    edges = edges.checked_add(ids.len())?;
                }
                SlotKind::Object(range) => {
                    let pairs = document.members.get(range.start..range.end)?;
                    for (name, child) in pairs {
                        if !inside(*child) {
                            return None;
                        }
                        totals.text = totals.text.checked_add(name.end - name.start)?;
                    }
                    totals.members = totals.members.checked_add(pairs.len())?;
                    edges = edges.checked_add(pairs.len())?;
                }
            }
        }
        (edges == root - first).then_some(Self {
            first,
            root,
            totals,
        })
    }
}

impl Document {
    /// A new document holding a copy of each subtree in `roots`, and the id
    /// of each copy in it, in the order of `roots`.
    ///
    /// The new document's root is an array whose element `i` is the copy of
    /// `roots[i]`; the returned id `i` names that copy (it is also the
    /// array's element `i`). Each copy has the same values, member order and
    /// number spellings as its source, so it encodes to the same canonical
    /// bytes. Every root is copied on its own: a root given twice, or a root
    /// inside another given root, is copied twice. That keeps each copy one
    /// contiguous run of ids and the size below a plain sum, with no lookup
    /// table for shared subtrees.
    ///
    /// This is how a caller keeps a few payloads of a large document without
    /// keeping the whole arena: extract them, rebind its handles to the
    /// returned ids, and drop the source.
    ///
    /// # Size and reservation
    ///
    /// A first pass counts the subtrees without allocating. The second asks
    /// `reservation` once per list, in the order decoded text, ranges, tags,
    /// array elements, object members, returned ids, each for that list's exact
    /// final capacity in bytes and before its only allocation. A list of zero
    /// bytes allocates nothing and is not asked for. With `T`, `S`, `I` and
    /// `M` the decoded text bytes (strings, number literals and member
    /// names), values, array elements and object members of the copied
    /// subtrees summed over `roots`, and `k = roots.len()`, the lists hold
    /// `T` bytes, `S + 1` values, `I + k` elements, `M` members and `k` ids.
    /// On a 64-bit target that is `T + 16(S + 1) + (S + 1) + 8(I + k) + 24M + 8k`
    /// bytes in all, and the returned document's
    /// [`Document::retained_bytes`] is the sum without the final `8k` ids term.
    /// Nothing depends
    /// on the parts of `self` outside `roots`.
    ///
    /// The walk is a scan of each subtree's id run: no recursion and no
    /// stack, whatever the depth. `self` is not changed.
    ///
    /// ```
    /// use quire_canonical::{read, to_vec, Limits, Reservation, ReservationDenied};
    ///
    /// struct Count(usize);
    /// impl Reservation for Count {
    ///     fn reserve(&mut self, bytes: usize) -> Result<(), ReservationDenied> {
    ///         self.0 += bytes;
    ///         Ok(())
    ///     }
    /// }
    ///
    /// let source = read(br#"{"big":[1,2,3],"keep":{"a":"x"}}"#, 1024).expect("reads");
    /// let keep = source.root().get("keep").expect("member").id();
    ///
    /// let mut charged = Count(0);
    /// let (payload, ids) = source.extract_with_reservation(&[keep], &mut charged)?;
    /// drop(source);
    ///
    /// let copy = payload.node(ids[0]).expect("copied");
    /// assert_eq!(to_vec(&copy, Limits::new(1024)).expect("encodes"), br#"{"a":"x"}"#);
    /// assert_eq!(to_vec(&payload, Limits::new(1024)).expect("encodes"), br#"[{"a":"x"}]"#);
    /// // Text "a" and "x"; 3 values; 1 element; 1 member; 1 id.
    /// # #[cfg(target_pointer_width = "64")]
    /// assert_eq!(charged.0, 2 + 16 * 3 + 3 + 8 + 24 + 8);
    /// # #[cfg(target_pointer_width = "64")]
    /// assert_eq!(payload.retained_bytes(), 2 + 16 * 3 + 3 + 8 + 24);
    /// # Ok::<(), quire_canonical::ExtractError>(())
    /// ```
    ///
    /// # Errors
    ///
    /// [`ExtractError::UnknownNode`] for the first root that is not a value
    /// of this document, before the reservation is asked anything;
    /// [`ExtractError::ReservationDenied`] when `reservation` refuses a list,
    /// after which nothing more is asked or allocated;
    /// [`ExtractError::Allocation`] when the allocator refuses a list or a
    /// size overflows `usize`. No document is returned on error.
    pub fn extract_with_reservation(
        &self,
        roots: &[NodeId],
        reservation: &mut dyn Reservation,
    ) -> Result<(Document, Vec<NodeId>), ExtractError> {
        let overflow = ExtractError::Allocation {
            requested: usize::MAX,
        };
        let mut sum = Totals::default();
        for root in roots {
            if root.0 >= self.ranges.len() {
                return Err(ExtractError::UnknownNode(*root));
            }
            // A document from `read` always has the layout `Span::of`
            // checks; one it cannot copy is refused, never copied wrongly.
            let span = Span::of(self, root.0).ok_or(ExtractError::UnknownNode(*root))?;
            sum = sum.add(span.totals).ok_or(overflow.clone())?;
        }
        let count = roots.len();
        let sum = sum
            .add(Totals {
                slots: 1,
                items: count,
                ..Totals::default()
            })
            .ok_or(overflow)?;

        let mut provision = Provision::new(reservation);
        let mut text = String::new();
        if sum.text > 0 {
            provision.charge(sum.text)?;
            text.try_reserve_exact(sum.text)
                .map_err(|_| ExtractError::Allocation {
                    requested: sum.text,
                })?;
        }
        let mut ranges: Vec<Range> = exact(&mut provision, sum.slots)?;
        let mut tags: Vec<SlotTag> = exact(&mut provision, sum.slots)?;
        let mut items: Vec<usize> = exact(&mut provision, sum.items)?;
        let mut members: Vec<(Range, usize)> = exact(&mut provision, sum.members)?;
        let mut ids: Vec<NodeId> = exact(&mut provision, count)?;

        for root in roots {
            // Counted above, so the span is there and every list has room.
            let span = Span::of(self, root.0).ok_or(ExtractError::UnknownNode(*root))?;
            let base = ranges.len();
            let remap = |id: usize| id - span.first + base;
            for id in span.first..=span.root {
                let slot = match self.slot(id).kind(self) {
                    SlotKind::Number { text: range, .. } => {
                        Slot::number(copy_text(&mut text, self.text_of(range)))
                    }
                    SlotKind::String(range) => {
                        Slot::string(copy_text(&mut text, self.text_of(range)))
                    }
                    SlotKind::Array(range) => {
                        let start = items.len();
                        let children = self.items.get(range.start..range.end).unwrap_or_default();
                        items.extend(children.iter().map(|child| remap(*child)));
                        Slot::array(Range {
                            start,
                            end: items.len(),
                        })
                    }
                    SlotKind::Object(range) => {
                        let start = members.len();
                        let pairs = self.members.get(range.start..range.end).unwrap_or_default();
                        for (name, child) in pairs {
                            let name = copy_text(&mut text, self.text_of(*name));
                            members.push((name, remap(*child)));
                        }
                        Slot::object(Range {
                            start,
                            end: members.len(),
                        })
                    }
                    SlotKind::Null => Slot::null(),
                    SlotKind::Bool(value) => Slot::boolean(value),
                };
                ranges.push(slot.range);
                tags.push(slot.tag);
            }
            ids.push(NodeId(remap(span.root)));
        }
        let start = items.len();
        items.extend(ids.iter().map(|id| id.0));
        let root = ranges.len();
        let root_slot = Slot::array(Range {
            start,
            end: items.len(),
        });
        ranges.push(root_slot.range);
        tags.push(root_slot.tag);
        Ok((
            Document {
                text,
                ranges,
                tags,
                items,
                members,
                root,
            },
            ids,
        ))
    }
}

/// An empty list with room for exactly `length` items, its bytes asked of
/// the reservation first; no request and no allocation when that is zero
/// bytes.
fn exact<T>(provision: &mut Provision<'_>, length: usize) -> Result<Vec<T>, ExtractError> {
    let mut list = Vec::new();
    let bytes = core::mem::size_of::<T>()
        .checked_mul(length)
        .ok_or(ExtractError::Allocation {
            requested: usize::MAX,
        })?;
    if bytes > 0 {
        provision.charge(bytes)?;
        list.try_reserve_exact(length)
            .map_err(|_| ExtractError::Allocation { requested: bytes })?;
    }
    Ok(list)
}

/// Append `piece` to `text`, whose capacity was reserved for it, and return
/// where it now sits.
fn copy_text(text: &mut String, piece: &str) -> Range {
    let start = text.len();
    text.push_str(piece);
    Range {
        start,
        end: text.len(),
    }
}
