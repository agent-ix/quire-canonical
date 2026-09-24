// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Agent-IX
//! String escaping, RFC 8785 §3.2.2.2 (the same as ECMAScript
//! `JSON.stringify`).
//!
//! `"` and `\` are escaped; `\b \t \n \f \r` use their two-character forms;
//! every other C0 control uses `\u00xx` with lowercase hex. Everything else,
//! including `U+007F` and all non-ASCII text, is emitted as literal UTF-8. No
//! Unicode normalization is applied.
//!
//! Every byte that needs escaping is ASCII, and UTF-8 continuation and lead
//! bytes are all `>= 0x80`, so scanning bytes and passing through unescaped
//! runs as whole slices never splits a character.

use crate::Error;

const HEX: &[u8; 16] = b"0123456789abcdef";

/// Pass the escaped form of `text` (no surrounding quotes) to `write`, as a
/// sequence of slices.
pub(crate) fn escape_fragment(
    text: &str,
    mut write: impl FnMut(&[u8]) -> Result<(), Error>,
) -> Result<(), Error> {
    let bytes = text.as_bytes();
    let mut run_start = 0;
    for (index, &byte) in bytes.iter().enumerate() {
        let mut control = [b'\\', b'u', b'0', b'0', 0, 0];
        let escaped: &[u8] = match byte {
            b'"' => b"\\\"",
            b'\\' => b"\\\\",
            0x08 => b"\\b",
            b'\t' => b"\\t",
            b'\n' => b"\\n",
            0x0c => b"\\f",
            b'\r' => b"\\r",
            0x00..=0x1f => {
                control[4] = HEX[usize::from(byte >> 4)];
                control[5] = HEX[usize::from(byte & 0x0f)];
                &control
            }
            _ => continue,
        };
        if let Some(run) = bytes.get(run_start..index) {
            if !run.is_empty() {
                write(run)?;
            }
        }
        write(escaped)?;
        run_start = index + 1;
    }
    match bytes.get(run_start..) {
        Some(run) if !run.is_empty() => write(run),
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::escape_fragment;

    fn escaped(text: &str) -> String {
        let mut out = Vec::new();
        escape_fragment(text, |bytes| {
            out.extend_from_slice(bytes);
            Ok(())
        })
        .expect("vec write is infallible");
        String::from_utf8(out).expect("escaping preserves UTF-8")
    }

    #[test]
    fn controls_use_short_forms_then_lowercase_hex() {
        assert_eq!(
            escaped("\u{8}\u{9}\u{a}\u{c}\u{d}\u{1f}\u{0}\u{f}"),
            "\\b\\t\\n\\f\\r\\u001f\\u0000\\u000f"
        );
    }

    #[test]
    fn del_non_ascii_and_solidus_are_literal() {
        assert_eq!(escaped("\u{7f}é😀/"), "\u{7f}é😀/");
    }

    #[test]
    fn only_quote_and_backslash_escape_among_printables() {
        assert_eq!(escaped("a\"b\\c"), "a\\\"b\\\\c");
        assert_eq!(escaped(""), "");
        assert_eq!(escaped("\"x"), "\\\"x");
    }
}
