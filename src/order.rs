// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Agent-IX
//! RFC 8785 §3.2.3 member order: by UTF-16 code unit.
//!
//! Rust's `str: Ord` compares Unicode scalar values, which is a different
//! order: `U+10000` (UTF-16 `D800 DC00`) sorts after `U+FFFD` by scalar value
//! and before it by code unit. The two agree everywhere below `U+D800`, so a
//! scalar-order encoder passes every ASCII vector and fails only on names that
//! mix supplementary-plane characters with `U+E000..=U+FFFF`.

use std::cmp::Ordering;

/// Compare two member names by UTF-16 code unit.
#[must_use]
pub(crate) fn cmp_utf16(left: &str, right: &str) -> Ordering {
    left.encode_utf16().cmp(right.encode_utf16())
}

#[cfg(test)]
mod tests {
    use std::cmp::Ordering;

    use super::cmp_utf16;

    #[test]
    fn supplementary_plane_sorts_before_high_bmp() {
        assert_eq!(cmp_utf16("\u{10000}", "\u{FFFD}"), Ordering::Less);
        assert_eq!(cmp_utf16("\u{1F600}", "\u{E000}"), Ordering::Less);
        // The scalar-value order Rust uses disagrees on both.
        assert_eq!("\u{10000}".cmp("\u{FFFD}"), Ordering::Greater);
        assert_eq!("\u{1F600}".cmp("\u{E000}"), Ordering::Greater);
    }

    #[test]
    fn prefix_sorts_first_and_equal_is_equal() {
        assert_eq!(cmp_utf16("a", "ab"), Ordering::Less);
        assert_eq!(cmp_utf16("ab", "a"), Ordering::Greater);
        assert_eq!(cmp_utf16("", ""), Ordering::Equal);
        assert_eq!(cmp_utf16("\u{1F600}", "\u{1F600}"), Ordering::Equal);
    }
}
