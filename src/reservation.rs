// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Agent-IX
//! A caller's say over the heap the writer reserves.
//!
//! A [`Reservation`] is asked for the bytes of every heap growth before the
//! growth happens, so a caller that meters memory charges exactly what the
//! writer allocates, by construction rather than by a separate estimate.

use alloc::string::String;
use alloc::vec::Vec;

use crate::Error;

/// The smallest capacity a writer-owned list grows to.
const MIN_CAPACITY: usize = 4;

/// A caller's decision on heap growth, asked before each growth.
///
/// Every growth of a writer-owned list calls [`Reservation::reserve`] once
/// with the full capacity, in bytes (element size times new capacity), that
/// the growth asks the allocator for. The writer grows to exactly the
/// capacity it computed, by doubling, so the bytes asked are the bytes
/// requested from the allocator. Calls are made in the order the growths
/// happen.
///
/// # Accounting unit and retention
///
/// The unit is bytes of list capacity, charged in full at each growth. A
/// growth that cannot extend in place holds the old and the new block at once
/// while it copies, and the full-capacity charge covers that moment; nothing
/// is credited back when the old block goes. So the sum of the bytes asked is
/// an upper bound on the writer's peak heap, equal to everything it requested
/// from the allocator. Capacity is kept until the writer is finished,
/// dropped, or has refused an event, and is reused across objects, so a
/// list is regrown only when it outgrows its capacity. The destination of
/// the output (the [`crate::Sink`]) is not the writer's heap and is never
/// asked about.
pub trait Reservation {
    /// Admit or refuse `bytes` of heap growth.
    ///
    /// # Errors
    ///
    /// [`ReservationDenied`] to refuse. The writer allocates nothing for the
    /// refused growth, drops everything it holds, and refuses the event with
    /// [`Error::ReservationDenied`]. Whatever else the caller needs to know
    /// about the denial (which budget, what remained) stays with the caller.
    fn reserve(&mut self, bytes: usize) -> Result<(), ReservationDenied>;
}

/// A [`Reservation`] refused a growth.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct ReservationDenied;

/// The writer's reservation, if its caller supplied one.
#[derive(Default)]
pub(crate) struct Provision<'r>(Option<&'r mut dyn Reservation>);

impl<'r> Provision<'r> {
    pub(crate) fn new(reservation: &'r mut dyn Reservation) -> Self {
        Self(Some(reservation))
    }

    /// Whether a caller supplied a reservation.
    pub(crate) fn is_metered(&self) -> bool {
        self.0.is_some()
    }

    /// Make room in `list` for `additional` more items, asking the
    /// reservation first.
    pub(crate) fn grow<T>(&mut self, list: &mut Vec<T>, additional: usize) -> Result<(), Error> {
        let size = core::mem::size_of::<T>();
        let Some(target) = self.admit(list.len(), list.capacity(), additional, size)? else {
            return Ok(());
        };
        list.try_reserve_exact(target - list.len())
            .map_err(|_| allocation::<T>(target - list.capacity()))
    }

    /// As [`Provision::grow`], for the bytes of `text`.
    pub(crate) fn grow_text(&mut self, text: &mut String, additional: usize) -> Result<(), Error> {
        let Some(target) = self.admit(text.len(), text.capacity(), additional, 1)? else {
            return Ok(());
        };
        text.try_reserve_exact(target - text.len())
            .map_err(|_| Error::Allocation {
                requested: target - text.capacity(),
            })
    }

    /// The capacity a list of `length` items and `capacity` must grow to for
    /// `additional` more, after the reservation admitted the whole new
    /// capacity; `None` when it already fits.
    fn admit(
        &mut self,
        length: usize,
        capacity: usize,
        additional: usize,
        item_size: usize,
    ) -> Result<Option<usize>, Error> {
        let Some(needed) = length.checked_add(additional) else {
            return Err(Error::Allocation {
                requested: item_size.saturating_mul(additional),
            });
        };
        if needed <= capacity {
            return Ok(None);
        }
        let target = needed.max(capacity.saturating_mul(2)).max(MIN_CAPACITY);
        self.charge(item_size.saturating_mul(target))?;
        Ok(Some(target))
    }

    /// Ask the reservation for `bytes` that the caller is about to allocate
    /// itself.
    pub(crate) fn charge(&mut self, bytes: usize) -> Result<(), Error> {
        if let Some(reservation) = self.0.as_deref_mut() {
            if reservation.reserve(bytes).is_err() {
                return Err(Error::ReservationDenied { requested: bytes });
            }
        }
        Ok(())
    }
}

fn allocation<T>(additional: usize) -> Error {
    Error::Allocation {
        requested: core::mem::size_of::<T>().saturating_mul(additional),
    }
}
