---
id: SR-1274
title: "QSL-219 gap analysis of quire-canonical PR #9 (out-of-range number pointer and lexeme)"
type: SpecReview
analysis: gap-analysis
scope: "agent-ix/quire-canonical@ded2d7ff96a1eeb8df9f280f144d2a9609820e66; PR #9 diff against origin/main: README.md, src/read.rs, tests/read.rs"
review_set: subset
---
# QSL-219 gap analysis of quire-canonical PR #9

## Summary

Ticket: QSL-219. quire-canonical has no spec of its own (`spec/` holds only
`.gitkeep`). I could not read the Linear ticket: `linear issue view QSL-219`
timed out. So the contract is the intent in the dispatch brief, which I
checked against the code:

- **An out-of-range number reports its JSON pointer.** Covered by
  `number_out_of_double_range_carries_pointer_and_lexeme`, with cases for an
  object member, an array element, the top level (empty pointer), a later
  index, escaped names with `~` and `/` (one from a `/` escape), and
  earlier sibling containers.
- **It reports its exact source text, so downstream can classify it as an
  inexact integer or an inexact number.** Covered by the same test, through
  sign, a fraction and an `E+` exponent (`-1e400`, `1.5E+999`). The variant
  doc states the classification rule that QSL applies.
- **Its offset is the number's first byte.** Covered: every case asserts the
  offset.
- **`1e-400` is accepted (it parses to 0.0) and keeps its text, as
  intended.** Covered by `number_underflow_is_accepted_with_its_text`.
- **`Malformed::NumberOutOfRange` is removed.** Removed, with no
  compatibility shim, and nothing else in the crate references it.

All production lines in the diff map to one of these items. There is no
underspecified code.

## Verdict

Complete. The single row below is the required placeholder. The tag issue is
recorded in SR-1273 FND-003.

## Findings

| ID | Severity | Summary | Refs |
| --- | --- | --- | --- |
| FND-001 | low | No findings (placeholder) | - |
