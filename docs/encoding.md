# Encoding details

The reference behind the [README](../README.md): how values reach the encoder,
how numbers are treated, what streams and what is buffered, and the feature
flags. The crate documentation (`make docs`) is the full API reference.

## Depth

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

## Sinks, ordering and refusals

`encode` writes into any `Sink` (a `sha2::Sha256`, a `Vec<u8>`, a
`WriteSink<impl io::Write>`, or a pair of sinks). Member names are sorted by
UTF-16 code unit by the encoder, so output does not depend on map backing. A
limit refuses the encoding; it never truncates.

The reader refuses malformed input with the byte offset of the fault. A number
with no finite double, such as `1e400`, also carries its RFC 6901 JSON pointer
and exact source text, in `ReadError::NumberOutOfRange`. An over-long input is
refused with its byte limit, which is never reported as malformed input.

## Streaming and memory

Arrays and scalars stream to the sink. While any object is open its canonical
bytes are buffered, so its members can be sorted; a top-level object is
therefore held whole before the first byte is hashed. The buffered bytes count
against the byte ceiling; see the crate docs for the full memory bound.

## Caller reservation

`Writer::with_reservation` (and `encode_with_reservation`) takes a
`Reservation`, a caller's say over the heap the writer holds besides the
output. The writer calls `Reservation::reserve(bytes)` once before each growth
of one of its internal lists (its container stack, the sort buffer, member and
closed-object records, the output walk's task stack, the held output) and
before the name allocation of a duplicate-member refusal, with the full
capacity in bytes that growth asks the allocator for. It then grows to exactly
that capacity, so the bytes asked are the bytes requested from the allocator,
and their sum bounds the writer's peak heap (a growth that copies holds the
old and the new block at once, and the full charge covers that). Capacity is
reused across objects, so an array of objects regrows nothing per element, and
is dropped when the writer is finished, dropped or refused. The `Sink` is the
caller's own destination and is never asked about.

An `Encode` implementation that keeps working lists of its own charges them
with `Writer::reserve_list` (grow a `Vec` through the reservation) or
`Writer::reserve` (charge bytes about to be allocated). The crate's own
`Encode` impls for `serde_json::Value`, `Document` and `NodeRef` do.

A `ReservationDenied` from the hook refuses the event with
`Error::ReservationDenied { requested }`. Nothing is allocated for the refused
growth and the writer drops everything it holds. A metered writer holds the
whole output and writes it to the sink in `finish`, so a refusal leaves the
sink untouched for every shape of value, top-level arrays and enum wrappers
included. That costs the output's length in heap (charged) and nothing else:
no second walk, and the output is written once.

Without a reservation the writer still streams: scalars and arrays reach the
sink as they are produced; a top-level object's opening brace reaches it at
once and its members when the object closes.
Two things differ from the writer before the reservation hook existed, for
every writer: lists grow by doubling from a minimum of 4 with `try_reserve_exact`
instead of `Vec`'s own policy, and a writer that refuses an event drops its
buffers at once instead of at drop.

## Reader working-byte provision

`read_with_reservation(input, max_input_bytes, reservation)` reads under the
same `Reservation` the writer takes, so one caller meter can sit behind both.
The encoded-input limit is checked first and before any parsing: an input over
it is `ReadError::Limit` (`LimitKind::InputBytes`) whatever it contains, and
the reservation is not asked anything. Past that, the reader asks
`Reservation::reserve(bytes)` once before each growth of one of its lists
(nodes, child ids, object members, decoded text, the open-container stack, the
children of open containers, the duplicate-name sort buffer, and the pointer
and lexeme of a number-out-of-range refusal, and the pointer of a
duplicate-name refusal). The unit is the writer's: the
bytes of the full new capacity, element size times new capacity, charged at
each growth and never credited back, with each list grown to exactly that
capacity. A denial is `ReadError::ReservationDenied { requested }`; nothing
was allocated for the growth, the reader drops everything it holds and no
document is returned. `read_bounded(input, max_input_bytes, max_working_bytes)`
is the same with a cumulative byte count, refusing with `ReadError::Limit`
(`LimitKind::WorkingBytes`, `bound` the provision, `required` the total
including the refused growth).

A `NumberOutOfRange` or `DuplicateName` refusal carries a charged JSON pointer
(and lexeme), which it keeps for as long as the error lives; they are not
part of the document. The scratch lists are dropped when the read returns; the `Document` keeps its
node, child, member and text lists, `Document::retained_bytes`. A meter that
does not refund already holds the document's charge; a refunding meter can
release down to `retained_bytes` and the rest when the document drops.
Decoding or copying the document afterwards is the caller's own growth,
charged by the caller to the same meter. There is no depth counter or fixed
size cap, and a document of any depth reads on a small thread stack.

## Numbers

A `Value`'s integers follow the integer rule, not `read`'s rule for JSON
text. A number serde_json holds as an `i64` or `u64` (from -2^63 to
2^64 - 1) is refused past 2^53, while `read` encodes every JSON number as
the double its text denotes: the literal `9007199254740993` is refused
through a `Value` and encodes as `9007199254740992` through `read`. An
integer literal past the 64-bit range is a float to serde_json, so it
encodes as its double through both, unless serde_json's
`arbitrary_precision` is on anywhere in the build: then a `Number` keeps its
literal text, and the `Value` path refuses such an integer instead. Floats
get ECMAScript number text.

## Features

The crate is `no_std` + `alloc`. The default `std` feature adds only
`WriteSink` and `Error::Sink`; with `default-features = false`, encoding into a
`Vec<u8>` or a hasher, `to_vec`, `sha256`, `sha256_with_domain`, the
`Writer` and the reader all work without `std`. `make build-no-std` builds
that configuration for `thumbv7em-none-eabi`.

The `serde_json` feature, off by default, implements `Encode` for
`serde_json::Value`. The value walks itself from an explicit heap stack and
pushes `Writer` events, so a value of any depth encodes on any thread stack.
serde_json is built with `alloc` rather than `std`, so this works without
`std`, and with `float_roundtrip`, so a float literal parses to its nearest
double and gives the same bytes through a `Value` as through `read` (feature
unification turns `float_roundtrip` on for every crate in a build that
enables this feature). serde_json's own `Drop` for `Value` recurses once per
level, so whoever owns a deep `Value` drops it with
`quire_canonical::drop_value`, which uses a heap stack.
