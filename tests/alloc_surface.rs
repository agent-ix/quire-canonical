// SPDX-License-Identifier: AGPL-3.0-or-later
// Copyright (C) 2026 Agent-IX
//! The `no_std` + `alloc` surface: encoding into an in-memory buffer and
//! minting a SHA-256 identity, with nothing from `std`.
//!
//! `make test-no-std` runs the suite with `--no-default-features`, so this
//! file then exercises the crate built without its `std` feature. The
//! expected digest was computed by `sha256sum` over the canonical text, not by
//! this crate.

use quire_canonical::{encode, sha256, to_vec, Limits, Sink};
use serde::Serialize;

const LIMITS: Limits = match Limits::new(1 << 10, 8) {
    Ok(limits) => limits,
    Err(_) => panic!("depth within MAX_DEPTH"),
};

/// A preimage shaped like a compound-unit identity record. Fields are declared
/// out of canonical order so the encoder's member sort is exercised.
#[derive(Serialize)]
struct Preimage {
    unit: &'static str,
    b: bool,
    a: (u8, f64, &'static str),
}

const PREIMAGE: Preimage = Preimage {
    unit: "quire.value.compound-unit/v1",
    b: true,
    a: (1, 2.5, "x"),
};

const CANONICAL: &[u8] = br#"{"a":[1,2.5,"x"],"b":true,"unit":"quire.value.compound-unit/v1"}"#;
const DIGEST: &str = "390c3ddcae1a21c9e1e2ea68d53114b167441060a9661a7ab1ef33804b9d7bdf";

#[test]
fn encodes_into_a_vec_and_mints_a_sha256_identity() {
    assert_eq!(to_vec(&PREIMAGE, LIMITS).expect("encodes"), CANONICAL);
    assert_eq!(
        sha256(&PREIMAGE, LIMITS).expect("hashes").to_string(),
        DIGEST
    );
}

#[test]
fn one_pass_feeds_a_vec_and_a_hasher_together() {
    let mut both = (Vec::new(), <sha2::Sha256 as sha2::Digest>::new());
    let written = encode(&mut both, &PREIMAGE, LIMITS).expect("encodes");
    assert_eq!(both.0, CANONICAL);
    assert_eq!(written, u64::try_from(CANONICAL.len()).expect("fits"));
    let digest: String = sha2::Digest::finalize(both.1)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    assert_eq!(digest, DIGEST);
}

/// A caller-defined sink works through the public trait alone.
#[test]
fn a_caller_sink_receives_the_canonical_bytes() {
    struct Counting(usize);
    impl Sink for Counting {
        fn write_bytes(&mut self, bytes: &[u8]) -> Result<(), quire_canonical::Error> {
            self.0 += bytes.len();
            Ok(())
        }
    }
    let mut sink = Counting(0);
    encode(&mut sink, &PREIMAGE, LIMITS).expect("encodes");
    assert_eq!(sink.0, CANONICAL.len());
}
