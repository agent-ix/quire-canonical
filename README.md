# quire-canonical

Streaming RFC 8785 (JCS) canonical JSON writer that hashes as it encodes.

## Use

```rust
use quire_canonical::{sha256, to_vec, Limits};

let limits = Limits::new(1 << 20, 128); // byte ceiling, nesting depth
let bytes = to_vec(&value, limits)?;    // RFC 8785 bytes
let digest = sha256(&value, limits)?;   // hashed while encoding, no buffer
```

`encode` writes into any `Sink` (a `sha2::Sha256`, a `Vec<u8>`, a
`WriteSink<impl io::Write>`, or a pair of sinks). Member names are sorted by
UTF-16 code unit by the encoder, so output does not depend on map backing or
`serde_json` features. A limit refuses the encoding; it never truncates.

## Build

```bash
make test
```

## License

AGPL-3.0-or-later
