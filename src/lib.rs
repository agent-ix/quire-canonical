//! Streaming RFC 8785 (JCS) canonical JSON writer that hashes as it encodes.

#![warn(missing_docs)]

/// Placeholder entry point.
pub fn hello() -> &'static str {
    "hello from quire_canonical"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hello_returns_greeting() {
        assert!(hello().contains("quire_canonical"));
    }
}
