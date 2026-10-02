---
id: SR-1240
title: "Code review of quire-canonical PR #6: Writer event API, iterative encoder, shared reader, FixedShape; MAX_DEPTH deleted"
type: SpecReview
analysis: code-review
scope: "agent-ix/quire-canonical@0955507f6a99f8782227a89741f86e089e700964; PR #6 diff against origin/main (9572a21): CLAUDE.md, README.md, src/{encoder,error,lib,number,read,shape,writer}.rs, tests/{alloc_surface,depth,encode,identity,limits,memory,read,vectors}.rs, tests/vectors/jcs-vectors.json"
review_set: subset
---
# Code review of quire-canonical PR #6

## Summary

Ticket: QSL-480 (slice QC1). Contract: QSL FR-259 "Capabilities QSL relies
on" and ADR-030 D-4.5, on quire-spec-language origin/main. quire-canonical
has no spec of its own, by team-leader ruling.

The PR replaces the recursive serde encoder with a push-event `Writer`. Every
path drives it: the serde path for `FixedShape` types, the arena tree from the
new `read`, and callers' own event sources through `Encode`. `MAX_DEPTH`,
`max_depth`, `DepthAboveMaximum` and `LimitKind::NestingDepth` are deleted.

What the coordinator asked to check:

- **No depth cap.** There is none. `FixedShape::DEPTH` is evaluated only in
  `const { T::DEPTH }` (src/encoder.rs:34) and never compared to anything.
  The writer's stack, frames, closed records and walk tasks each stand for
  produced bytes, and `deep_opens_are_refused_on_bytes_not_depth`,
  `nested_object_buffers_are_refused_on_bytes` and
  `deep_document_encoding_is_refused_on_bytes` show the byte refusal. The
  reader checks its input byte limit before any work
  (`reader_input_byte_limit_is_checked_first_and_is_not_malformed_input`).
- **Integers past 2^53 in JSON text.** Encoding them as their nearest double
  is what RFC 8785 does: it canonicalizes the IEEE 754 value of each number,
  the way ECMAScript's `JSON.parse` reads it, and I-JSON only says such
  numbers SHOULD NOT be sent. No digest that QSL relies on today can change.
  The old encoder refused these integers when they fit `u64`/`i64`, and
  already rounded them when they did not. QSL's intake digest
  (`Rfc8785Numbers`, qsl-semantics/src/model/intake.rs:507-547) already rounds
  them to the nearest double itself. Rust integer values past 2^53 are still
  refused on the serde path and by `Writer::integer`.
- **Canonical output.** I ran a differential outside the repo. I generated
  200,000 random JSON documents: nested objects and arrays, escapes, astral
  characters, long-mantissa floats, subnormals, and integers on both sides of
  2^53. Each went through old `main` (`serde_json` with `float_roundtrip`,
  then `to_vec`) and through this head (`read`, then `to_vec`). The old crate
  accepted 174,625 of them, and all 174,625 are byte-identical. The same
  174,625 are also byte-identical through this head's serde path. The other
  25,375 were all old `IntegerMagnitudeAboveMaximum` refusals; this head
  accepts them as nearest doubles. The golden vectors pass unchanged.
- **Encoder performance.** It is linear. On a release build, a deep
  `{"head":n,"tail":...}` chain takes 72 ms at 100k, 139 ms at 200k, 282 ms at
  400k and 674 ms at 800k. A flat object of objects grows as n log n, from the
  member sort. Nothing is quadratic: each byte is buffered once and written
  once, and each closed child is copied once. The 4x peak-heap bound holds
  only for the flat shape (FND-002).
- **Rust lane (rust-review).** No new `unsafe`. No indexing or `unwrap` on a
  production path. Every heap structure grows through `try_reserve`. Integer
  conversions are checked (`buffer_offset`, `index_u32`). `make ci` ran in a
  detached worktree at this head and passed (fmt, clippy -D warnings, all
  test lanes, no_std build, deny, unsafe audit, docs). `const { }` needs Rust
  1.79, and the crate's MSRV is 1.81.
- Test oracles are strong where it matters. depth.rs compares the deep digest
  with SHA-256 over canonical text built independently. The event-API
  refusal tests assert the exact `ProtocolViolation` and the `AfterRefusal`
  that follows it. The vectors still carry their independent canonical bytes
  and digests.
- Ceremony removed (the vector-file `version`, the inventory/count tests and
  `refused_integers`) is removed correctly. Two things still cover the
  behaviour: `json_text_integers_encode_as_their_double` covers the reader,
  and `integers_past_two_pow_53_in_magnitude_are_refused` still covers the
  refusal of Rust integers.

## Verdict

Changes requested: two medium and three low findings. Each fix is a doc or
test change inside this PR. Canonical output and behaviour are sound.

## Findings

| ID | Severity | Summary | Refs |
| --- | --- | --- | --- |
| FND-001 | medium | `FixedShape` blocks recursion only when `DEPTH` is computed with `nest` from its fields' `DEPTH`. A literal `DEPTH` compiles for any type. Reproduced: `struct Lying(serde_json::Value)` with `const DEPTH: usize = 1` compiles, and `to_vec` on a 50,000-deep value aborts the process with a stack overflow on a 256 KiB thread. So the README and crate docs claim more than the code enforces when they say "a recursive type cannot implement `FixedShape` without a compile-time cycle". FR-259 capability 4 ("a serde encoding path that only fixed-depth types can take") holds only if implementers follow that convention. Fix: state the rule in the docs (`DEPTH` must be `nest` over every field's `DEPTH`; a literal on a type with fields defeats the check), and offer a `#[derive(FixedShape)]` so the computed form is the default one. | README.md:23-26; src/lib.rs:24-27; src/shape.rs:15-23 |
| FND-002 | medium | The writer docs say `tests/memory.rs` "measures the peak heap of the worst shape, a flat object of small members, and keeps it under 4x". The flat object is not the worst shape. Measured on a release build: a deep `{"k":{"k":...}}` chain peaks at 25.6x, 38.8x and 31.9x its canonical length at 1k, 10k and 100k levels, because each open object has a 64-byte `Frame` plus `Vec` slack. A flat object of 100k empty objects peaks at 5.3x, and at 7.3x at 10k. Memory stays linear in bytes, but a caller who sizes memory as 4x `max_bytes` is off by about 10x. Fix: state the per-shape bound truthfully (per open object and per closed child), or shrink `Frame` (for example, allocate its `members`/`children` lazily). Add a deep-object case to tests/memory.rs. | src/writer.rs:44-55; src/lib.rs:59-64 |
| FND-003 | low | The reader says the input byte limit "bounds memory as well as time", but gives no constant. Measured peak heap: 46-73x the input for `[[[...]]]` and 42-64x for `[1,1,...]`. That comes from a 48-byte `OpenContainer` per `[`, plus a 32-byte `Slot` and a 40-byte scratch `Child` per value, before `Vec` slack. A 16 MiB intake input can reach about 1 GiB. Fix: document the multiplier, or narrow `Range`/ids to `u32` to cut it. | src/read.rs:13-21 |
| FND-004 | low | The `compile_fail` doctest that backs "a recursive type fails to compile" does not pin its error code. It passes for any compile error, including an unrelated one such as a broken import. I confirmed the snippet fails today with E0391 (cycle detected). Fix: change the fence to `compile_fail,E0391`. | src/shape.rs:25 |
| FND-005 | low | The derived `PartialEq` on `Document` compares arena layout, not JSON values. `{"a":1}` and `{"a":1.0}`, or `{"a":1,"b":2}` and `{"b":2,"a":1}`, are unequal, though each pair has the same RFC 8785 text. The docs say only that comparing never recurses. Fix: say that equality is structural (member order and number spelling included), and that equal canonical bytes are the value equality. | src/read.rs:136-141 |
