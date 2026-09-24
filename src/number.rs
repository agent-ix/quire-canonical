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
//! their double value. An integer the double cannot hold exactly is refused,
//! because rounding it would silently change the value being identified.

use crate::Error;

/// `2^53`: every integer with a smaller odd part is exactly a double.
const DOUBLE_SIGNIFICAND_LIMIT: u128 = 1 << 53;

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

/// The double equal to an integer of the given sign and magnitude, if one
/// exists.
///
/// An integer is exactly a double when its odd part (the magnitude with its
/// trailing zero bits removed) fits the 53-bit significand; the exponent range
/// covers every `u128`.
pub(crate) fn exact_double(negative: bool, magnitude: u128) -> Option<f64> {
    if magnitude == 0 {
        return Some(0.0);
    }
    let odd_part = magnitude >> magnitude.trailing_zeros();
    if odd_part >= DOUBLE_SIGNIFICAND_LIMIT {
        return None;
    }
    // Exact: the odd-part check above proves the double holds this integer.
    #[allow(clippy::cast_precision_loss)] // reason: exactness proven above.
    let value = magnitude as f64;
    Some(if negative { -value } else { value })
}

#[cfg(test)]
mod tests {
    use super::{exact_double, with_double_text};
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
    fn exact_double_accepts_only_representable_integers() {
        assert_eq!(exact_double(false, 1 << 53), Some(9_007_199_254_740_992.0));
        assert_eq!(exact_double(false, (1 << 53) + 1), None);
        assert_eq!(
            exact_double(true, (1 << 53) - 1),
            Some(-9_007_199_254_740_991.0)
        );
        // Large but exact: a single set bit.
        assert_eq!(exact_double(false, 1 << 100), Some(2.0_f64.powi(100)));
        assert_eq!(exact_double(false, u128::MAX), None);
        assert_eq!(exact_double(false, u128::from(u64::MAX)), None);
        assert_eq!(exact_double(false, 0), Some(0.0));
    }
}
