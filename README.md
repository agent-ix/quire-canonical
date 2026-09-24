# quire-canonical

Streaming RFC 8785 (JCS) canonical JSON writer that hashes as it encodes.

## Use

```rust
use quire_canonical::{sha256, to_vec, Limits};

let limits = Limits::new(1 << 20, 128)?; // byte ceiling, nesting depth (<= Limits::MAX_DEPTH)
let bytes = to_vec(&value, limits)?;     // RFC 8785 bytes
let digest = sha256(&value, limits)?;    // hashed while encoding
```

`encode` writes into any `Sink` (a `sha2::Sha256`, a `Vec<u8>`, a
`WriteSink<impl io::Write>`, or a pair of sinks). Member names are sorted by
UTF-16 code unit by the encoder, so output does not depend on map backing or
`serde_json` features. A limit refuses the encoding; it never truncates.

Arrays and scalars stream to the sink. Each open object buffers its members'
canonical bytes until it closes, so it can sort them; a top-level object is
therefore held whole before the first byte is hashed. The buffered bytes count
against the byte ceiling; see the crate docs for the full memory bound.

## Build

```bash
make test
```

## License

AGPL-3.0-or-later
