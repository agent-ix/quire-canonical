# quire-canonical

Streaming RFC 8785 (JCS) canonical JSON writer that hashes as it encodes.

## Use

```rust
use quire_canonical::{read, sha256, to_vec, Limits, Writer};

let limits = Limits::new(1 << 20);           // canonical byte ceiling; no depth limit
let bytes = to_vec(&fixed_shape_value, limits)?; // a `FixedShape` type, through serde
let document = read(json_bytes, 1 << 20)?;   // untrusted JSON into an arena tree
let digest = sha256(&document, limits)?;     // hashed while encoding
let bytes = to_vec(&json_value, limits)?;    // a serde_json::Value (`serde_json` feature)

let mut writer = Writer::new(&mut sink, limits); // push events from your own stack
writer.begin_array()?;
writer.integer(1)?;
writer.end_array()?;
writer.finish()?;
```

Nothing recurses in proportion to its input and nothing bounds depth: only
byte limits apply. A value reaches the encoder through the `Writer` event API
(data whose depth follows its input), as a `Document` from the reader, as a
`serde_json::Value` (with the `serde_json` feature), or through serde for a
`FixedShape` type, whose depth is fixed by its schema.
`#[derive(FixedShape)]` computes `DEPTH` from every field's `DEPTH`, so a
recursive type that derives it is a compile-time cycle (E0391). A hand-written
impl gets the same check only if its `DEPTH` is `nest` over every field's
`DEPTH`; a literal `DEPTH` defeats it, and a recursive value can then overflow
the stack through serde.

`encode` writes into any `Sink` (a `sha2::Sha256`, a `Vec<u8>`, a
`WriteSink<impl io::Write>`, or a pair of sinks). Member names are sorted by
UTF-16 code unit by the encoder, so output does not depend on map backing. A
limit refuses the encoding; it never truncates. The reader refuses malformed
input with the byte offset of the fault, and an over-long input with its byte
limit, which is never reported as malformed input.

The crate is `no_std` + `alloc`. The default `std` feature adds only
`WriteSink` and `Error::Sink`; with `default-features = false`, encoding into a
`Vec<u8>` or a hasher, `to_vec`, `sha256`, `sha256_with_domain`, the
`Writer` and the reader all work without `std`. `make build-no-std` builds
that configuration for `thumbv7em-none-eabi`.

The `serde_json` feature, off by default, implements `Encode` for
`serde_json::Value`. The value walks itself from an explicit heap stack and
pushes `Writer` events, so a value of any depth encodes on any thread stack.
Integers go through the integer rules and are refused past 2^53; floats get
ECMAScript number text. serde_json is built with only its `alloc` feature, so
this works without `std` as well. serde_json's own `Drop` for `Value` recurses
once per level, so whoever owns a deep `Value` must drop it from a heap stack.

Arrays and scalars stream to the sink. While any object is open its canonical
bytes are buffered, so its members can be sorted; a top-level object is
therefore held whole before the first byte is hashed. The buffered bytes count
against the byte ceiling; see the crate docs for the full memory bound.

## Build

```bash
make test
```

## License

AGPL-3.0-or-later
