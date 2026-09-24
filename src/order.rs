// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Agent-IX
//! RFC 8785 §3.2.3 member order: by UTF-16 code unit, compared directly on the
//! encoder's own escaped output.
//!
//! Members are buffered as their canonical bytes (`"name":value`), and sorting
//! reads the name back out of those bytes rather than keeping a second,
//! unescaped copy. [`Unescaped`] undoes exactly the escapes
//! [`crate::escape`] emits; the result is the name's UTF-8 bytes.
//!
//! UTF-8 byte order is Unicode scalar-value order, and that differs from
//! UTF-16 code-unit order in one place only: a supplementary-plane character
//! (UTF-16 surrogates `D800..DBFF`) sorts *before* `U+E000..=U+FFFF` by code
//! unit and *after* it by scalar value. Two strings with an equal prefix are
//! at the same character boundary, so their first differing bytes are either
//! both lead bytes or both continuation bytes. Only lead bytes can straddle
//! that gap — `0xEE`/`0xEF` start `U+E000..=U+FFFF`, `0xF0..=0xF4` start the
//! supplementary planes — and those two cases are inverted.

use std::cmp::Ordering;

/// The unescaped UTF-8 bytes of a canonical JSON string, read from just after
/// its opening quote up to its closing quote.
pub(crate) struct Unescaped<'a> {
    bytes: &'a [u8],
    index: usize,
}

impl<'a> Unescaped<'a> {
    /// The name of a buffered member: `member` starts with the name's opening
    /// quote.
    pub(crate) fn member_name(member: &'a [u8]) -> Self {
        Self {
            bytes: member,
            index: 1,
        }
    }
}

impl Iterator for Unescaped<'_> {
    type Item = u8;

    fn next(&mut self) -> Option<u8> {
        let byte = *self.bytes.get(self.index)?;
        match byte {
            b'"' => None,
            b'\\' => {
                let escape = *self.bytes.get(self.index + 1)?;
                self.index += 2;
                Some(match escape {
                    b'b' => 0x08,
                    b't' => b'\t',
                    b'n' => b'\n',
                    b'f' => 0x0c,
                    b'r' => b'\r',
                    b'u' => {
                        let digits = self.bytes.get(self.index..self.index + 4)?;
                        self.index += 4;
                        let text = std::str::from_utf8(digits).ok()?;
                        // The escaper emits `\u00xx` only for C0 controls.
                        u8::from_str_radix(text.get(2..)?, 16).ok()?
                    }
                    // `\"` and `\\`.
                    other => other,
                })
            }
            _ => {
                self.index += 1;
                Some(byte)
            }
        }
    }
}

/// Compare two buffered members by their names' UTF-16 code units.
pub(crate) fn cmp_member_names(left: &[u8], right: &[u8]) -> Ordering {
    let mut left = Unescaped::member_name(left);
    let mut right = Unescaped::member_name(right);
    loop {
        match (left.next(), right.next()) {
            (None, None) => return Ordering::Equal,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(a), Some(b)) if a == b => {}
            (Some(a), Some(b)) => return utf16_order_of_first_difference(a, b),
        }
    }
}

fn utf16_order_of_first_difference(left: u8, right: u8) -> Ordering {
    let high_bmp = |byte: u8| byte == 0xEE || byte == 0xEF;
    let supplementary = |byte: u8| byte >= 0xF0;
    if high_bmp(left) && supplementary(right) {
        Ordering::Greater
    } else if supplementary(left) && high_bmp(right) {
        Ordering::Less
    } else {
        left.cmp(&right)
    }
}

#[cfg(test)]
mod tests {
    use super::{cmp_member_names, Unescaped};
    use crate::escape::escape_fragment;

    fn member(name: &str) -> Vec<u8> {
        let mut out = vec![b'"'];
        escape_fragment(name, |run| {
            out.extend_from_slice(run);
            Ok(())
        })
        .expect("vec write");
        out.extend_from_slice(b"\":0");
        out
    }

    const SAMPLES: [&str; 22] = [
        "",
        "\u{0}",
        "\u{8}",
        "\n",
        "\u{1f}",
        "\"",
        "\\",
        "a",
        "~",
        "\u{7f}",
        "\u{80}",
        "\u{7ff}",
        "\u{800}",
        "\u{d7ff}",
        "\u{e000}",
        "\u{ff21}",
        "\u{fffd}",
        "\u{ffff}",
        "\u{10000}",
        "\u{1f600}",
        "\u{10ffff}",
        "ab",
    ];

    #[test]
    fn unescaping_recovers_every_sample_name() {
        for name in SAMPLES {
            let bytes = member(name);
            let decoded: Vec<u8> = Unescaped::member_name(&bytes).collect();
            assert_eq!(decoded, name.as_bytes(), "{name:?}");
        }
    }

    /// Every pair of one- and two-character names orders exactly as the
    /// reference UTF-16 comparison does.
    #[test]
    fn agrees_with_utf16_code_unit_order_on_every_sample_pair() {
        let mut names: Vec<String> = SAMPLES.iter().map(|name| (*name).to_owned()).collect();
        for first in SAMPLES {
            for second in SAMPLES {
                names.push(format!("{first}{second}"));
            }
        }
        for left in &names {
            for right in &names {
                let expected = left.encode_utf16().cmp(right.encode_utf16());
                assert_eq!(
                    cmp_member_names(&member(left), &member(right)),
                    expected,
                    "{left:?} vs {right:?}"
                );
            }
        }
    }

    #[test]
    fn supplementary_plane_sorts_before_high_bmp() {
        use std::cmp::Ordering;
        assert_eq!(
            cmp_member_names(&member("\u{10000}"), &member("\u{FFFD}")),
            Ordering::Less
        );
        // Scalar-value (and UTF-8 byte) order disagrees.
        assert_eq!("\u{10000}".cmp("\u{FFFD}"), Ordering::Greater);
    }
}
