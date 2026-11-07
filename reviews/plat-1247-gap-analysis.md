---
id: SR-3407
title: "Gap analysis of quire-canonical PR 18 against PLAT-1247 acceptance checks"
type: SpecReview
analysis: gap-analysis
scope: "agent-ix/quire-canonical; PR 18 head 57fcb89e47fcaa1aa427219a2cf8d1e889741984 diff against origin/code/plat1265-writer-preflight; ticket PLAT-1247 acceptance checks 1-5"
review_set: subset
---

## Summary

Ticket: PLAT-1247. PR: quire-canonical#18.

quire-canonical has no spec artifacts (`spec/` is empty; earlier ruling
recorded in SR-1241 and SR-2475). The normative text is docs/encoding.md and
the module docs, so a planless `quoin matrix` has no criteria to trace. This
is a manual check of the ticket's five acceptance checks against the tests.
Plan completion: not assessed. Tools: quoin 0.29.4, quire 0.38.3.

AC to test trace:

- **AC1** (encoded-input provision refuses before parsing, malformed beyond
  it too): met. `encoded_limit_comes_before_parsing` covers valid,
  truncated, invalid UTF-8 and duplicate-name inputs over the limit, asserts
  `InputBytes`, bound, required and that the reservation was never asked,
  for both entry points.
- **AC2** (working denial before node / decoded text / member-item /
  open-container growth; typed limit with required; no partial document):
  met for node, unescaped text, items, members, open containers and the
  sort buffer. The deny-each-request sweep asserts the denied bytes, live
  heap 0 and no further requests; `bounded_reads_name_the_limit_and_required_bytes`
  asserts `WorkingBytes`, bound and required at every request. Escaped text
  and the out-of-range pointer are not exercised (FND-001).
- **AC3** (in-budget identity of tree, canonical bytes and typed refusals):
  met. Document equality and `to_vec` bytes across plain, metered and
  exact-provision reads; duplicate name, invalid UTF-8, invalid escape,
  malformed number, trailing content and NumberOutOfRange are identical.
- **AC4** (deep inputs on a small stack, success and denial, no depth
  counter): met. 100,000-deep arrays and objects on a 256 KiB thread, exact
  provision succeeds, one under refuses with `WorkingBytes`. No depth counter
  was added.
- **AC5** (composable accounting, unit and retention stated, tests observe
  decisions): met. `read_with_reservation` takes the writer's `Reservation`,
  the unit and release/retention are stated, `retained_bytes` equals live
  heap after the read (tested), and the tests record actual reservation
  requests rather than inferring from input length.

## Verdict

**PASS WITH FINDINGS**: all five checks are met; one coverage gap in AC2.

## Findings

| ID | Severity | Summary | Refs |
| --- | --- | --- | --- |
| FND-001 | medium | AC2 decoded-text growth is tested only for unescaped runs, and the NumberOutOfRange pointer/lexeme growth only by variant. Mutation probes removing the charge on the escape path and on the pointer `steps` list leave the suite green (see SR-3406 FND-001, FND-002). Add an escaped-string shape (e.g. `"a\\nb\\u00e9..."`) to SHAPES and run the deny sweep over `[[1e400]]` | tests/read_reservation.rs:112-119, tests/read_reservation.rs:258-268, src/read.rs:1100, src/read.rs:720 |

## Dispositions

Round 1, PR head 26b4d28905eecd86cb6272938f5d74ada093f2d9. AC1-AC5 remain
met. AC3 now carries the duplicate-name refusal as
`ReadError::DuplicateName { offset, pointer }` (typed and located), identical
metered and unmetered. No new findings.

| FND | outcome | reason |
| --- | --- | --- |
| FND-001 | fixed | Escaped text and escaped names shapes run the full charge, peak and deny sweeps; the NumberOutOfRange and DuplicateName pointer paths have exact or swept request checks; mutations go red |
