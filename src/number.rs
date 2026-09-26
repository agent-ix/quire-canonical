// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Agent-IX
//! Number text: ECMAScript `Number::prototype.toString`, as RFC 8785
//! §3.2.2.3 adopts it.
//!
//! Rust's own float formatting differs from it in the places that change a
//! digest: `1e21` versus `1e+21`, `1e20` versus `100000000000000000000`, and
//! `-0` versus `0`. `ryu-js` implements the ECMAScript algorithm.
//!
//! Every RFC 8785 number is an IEEE 754 double, so integers are encoded by
//! their double value. Below [`MAX_EXACT_INTEGER_MAGNITUDE`] every magnitude
//! holds exactly in a double; above it, some magnitudes still land on an
//! exact double (`2^60`, `2^100`, any value with enough trailing zero bits)
//! while others round. Rather than accept that some-exact-some-not split, the
//! encoder refuses every integer past the bound, exact or not, naming the
//! value.
//!
//! This bound is enforced only for Rust integer types (`i8`..`i128`,
//! `u8`..`u128`) reaching the encoder through its `serialize_i*`/
//! `serialize_u*` methods. A JSON-text integer already too large for
//! `u64`/`i64` is not one of these: `serde_json` itself parses it straight to
//! an `f64` (`serde_json::Number::Float`), so it reaches this crate as a
//! double and is encoded as one, like any other float — see
//! `tests/encode.rs` for the pinned behaviour.

use crate::Error;

/// `2^53`: the largest integer magnitude every smaller magnitude, and this
/// one, holds exactly as a double — and the bound past which every integer is
/// refused rather than encoded, exact or not. (ECMAScript's
/// `Number.MAX_SAFE_INTEGER` is `2^53 - 1`, one less than this bound; this
/// crate's bound is inclusive of `2^53` itself, so the name says "exact", not
/// "safe".)
pub(crate) const MAX_EXACT_INTEGER_MAGNITUDE: u128 = 1 << 53;

/// ECMAScript text for a finite double. Both zeros print as `0`.
pub(crate) fn with_double_text<R>(
    value: f64,
    write: impl FnOnce(&[u8]) -> Result<R, Error>,
) -> Result<R, Error> {
    if !value.is_finite() {
        return Err(Error::NonFiniteNumber(value));
    }
    if value == 0.0 {
        return write(b"0");
    }
    let mut buffer = ryu_js::Buffer::new();
    write(buffer.format_finite(value).as_bytes())
}

/// The double for an integer of the given sign and magnitude, or `None` when
/// `magnitude` exceeds [`MAX_EXACT_INTEGER_MAGNITUDE`] and is refused rather
/// than encoded.
pub(crate) fn exact_integer_double(negative: bool, magnitude: u128) -> Option<f64> {
    if magnitude > MAX_EXACT_INTEGER_MAGNITUDE {
        return None;
    }
    if magnitude == 0 {
        return Some(0.0);
    }
    // Exact: every magnitude up to MAX_EXACT_INTEGER_MAGNITUDE (2^53) fits
    // the 53-bit significand.
    #[allow(clippy::cast_precision_loss)] // reason: exactness proven above.
    let value = magnitude as f64;
    Some(if negative { -value } else { value })
}

#[cfg(test)]
mod tests {
    use super::{exact_integer_double, with_double_text};
    use crate::Error;

    fn text(value: f64) -> String {
        with_double_text(value, |bytes| {
            Ok(String::from_utf8_lossy(bytes).into_owned())
        })
        .unwrap_or_else(|error| format!("error: {error}"))
    }

    #[test]
    fn follows_ecmascript_not_rust_display() {
        assert_eq!(text(-0.0), "0");
        assert_eq!(text(1e20), "100000000000000000000");
        assert_eq!(text(1e21), "1e+21");
        assert_eq!(text(1e-6), "0.000001");
        assert_eq!(text(1e-7), "1e-7");
        assert_eq!(text(1.0), "1");
    }

    #[test]
    fn refuses_non_finite() {
        for value in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            assert!(matches!(
                with_double_text(value, |_| Ok(())),
                Err(Error::NonFiniteNumber(_))
            ));
        }
    }

    #[test]
    fn exact_integer_double_accepts_only_magnitudes_up_to_two_pow_53() {
        assert_eq!(
            exact_integer_double(false, 1 << 53),
            Some(9_007_199_254_740_992.0)
        );
        assert_eq!(exact_integer_double(false, (1 << 53) + 1), None);
        assert_eq!(
            exact_integer_double(true, (1 << 53) - 1),
            Some(-9_007_199_254_740_991.0)
        );
        // Exact as a double, but past the magnitude bound: refused anyway.
        assert_eq!(exact_integer_double(false, 1 << 100), None);
        assert_eq!(exact_integer_double(false, u128::MAX), None);
        assert_eq!(exact_integer_double(false, u128::from(u64::MAX)), None);
        assert_eq!(exact_integer_double(false, 0), Some(0.0));
    }
}
