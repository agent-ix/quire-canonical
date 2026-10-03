---
id: SR-1273
title: "Code review of quire-canonical PR #9: out-of-range number read error carries JSON pointer and lexeme (QSL-219)"
type: SpecReview
analysis: code-review
scope: "agent-ix/quire-canonical@ded2d7ff96a1eeb8df9f280f144d2a9609820e66; PR #9 diff against origin/main: README.md, src/read.rs, tests/read.rs"
review_set: subset
---
# Code review of quire-canonical PR #9

## Summary

Ticket: QSL-219. PR: quire-canonical#9. The PR deletes
`Malformed::NumberOutOfRange` and adds `ReadError::NumberOutOfRange { offset,
pointer, lexeme }`. `ReadError` is no longer `Copy`. The pointer is built from
the parser's open-container stack, and only on the error path. Rust lane
(rust-review) is folded into this file.

What the brief asked to check:

- **RFC 6901 correctness.** Correct. For an array, the index is the number of
  completed children in that array's `scratch_items` slice. The slice ends at
  the next open array's `start`, and objects never push to `scratch_items`.
  For an object, the step is the last `PendingMember` before the next open
  object's `start`. A closed sibling container has already been truncated
  away, so it shifts nothing. Each character of a decoded name is escaped
  separately (`~` to `~0`, `/` to `~1`), so the escape order cannot go wrong.
  The pointer is empty at the top level. I ran a probe crate outside the repo
  against this head with `default-features = false`. Results: `{"": 1e400}`
  gives `/`. `[[1,[2]],{"a":[3]},[], 1e400]` gives `/3`.
  `{"a":1,"b":{"c":[0,{"d":1e999}]}}` gives `/b/c/1/d`. `{"é~/": 1e400}`
  gives `/é~0~1`. The PR's own test covers `/` decoding to `/` and then
  escaping to `~1`.
- **Iterative read loop, and no overflow or panic on the error path.** The
  read loop is unchanged. `pointer()` is one loop over `stack` plus one loop
  over `steps`, with no recursion. Every allocation is fallible: `push` uses
  `try_reserve`, the pointer `String` uses `try_reserve`, and `owned` uses
  `try_reserve`. Arithmetic is saturating, except `steps.len() * 21`, which
  cannot overflow because a `Vec<Step>` holds at most `isize::MAX / 16`
  entries. `unwrap_or_default` stands in where code would otherwise index.
  The probe read a number nested in 200,000 arrays and a number nested in
  200,000 objects. Both returned a 400,000-byte pointer with no stack
  overflow.
- **no_std / feature matrix.** `make lint` passed all four clippy lanes with
  `-D warnings`: default, all-features, no-default-features, and
  thumbv7em-none-eabi lib. `make build-no-std` passed with and without
  `serde_json`. `String` comes from `alloc::string`.
- **API idiom.** The struct variant with named fields is fine under
  `#[non_exhaustive]`, and so is dropping `Copy` (the crate is
  `publish = false` and pre-release). The message reads well: `number 1e400
  at "/0" (byte 1) has no finite IEEE 754 double`. One doc gap (FND-001).
- **Test oracles.** These are real. The oracles are literal offsets,
  pointers and lexemes, and I checked the offset 30 in the escaped-name case
  by hand. The sibling-container case covers both the array slice and the
  object truncation. The underflow test asserts the source text `1e-400` and
  the value `0.0`, which matches the intended behaviour of accepting it.
- **Focused run** under the build lock: `cargo test --test read` passed 9
  tests.

## Verdict

Approve with three low findings. None of them changes behaviour. The pointer
logic is correct, iterative and fallible.

## Findings

| ID | Severity | Summary | Refs |
| --- | --- | --- | --- |
| FND-001 | low | The `# Errors` section of `read` still lists only `Limit`, `Malformed` ("with the byte offset of the fault otherwise") and `Allocation`. It does not list the new `ReadError::NumberOutOfRange`, so the rustdoc tells a caller that an out-of-range number is a `Malformed`. Fix: add the variant to the list. | src/read.rs:452-457 |
| FND-002 | low | `pointer()` reserves `text.len() * 2 + steps * 21` bytes. That is twice all the decoded text read so far, not twice the names on the path. On a large document, the error path asks for up to twice the document's size, and under memory pressure the `NumberOutOfRange` turns into an `Allocation` error. The `Allocation { requested }` it reports is `self.text.len()`, not the amount actually requested. Fix: sum the escaped length of each name step and the digits of each index step, then reserve exactly that (or reserve per step), and report the real request. | src/read.rs:624-634 |
| FND-003 | low | The test doc tag changed from the requirement id `FR-259` to the ticket id `QSL-219` on `number_out_of_double_range_carries_pointer_and_lexeme` and `number_underflow_is_accepted_with_its_text`. A ticket id is not a requirement. The refusal is still the FR-259 behaviour that the sibling test (`FR-259: a lone surrogate escape...`) traces. Fix: keep `FR-259` (plus the QSL-side FR/AC that the pointer and lexeme serve, if one exists). | tests/read.rs:54, tests/read.rs:77 |
