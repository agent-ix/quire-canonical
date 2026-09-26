---
id: SR-002
title: "Gap analysis — PLAT-1074 acceptance criteria to tests (quire-canonical#2)"
type: SpecReview
analysis: gap-analysis
scope: "agent-ix/quire-canonical@e7353e227196b8f576910faac375595bcbe403b2; src/encoder.rs, src/number.rs, src/lib.rs, src/error.rs, tests/encode.rs, tests/limits.rs, tests/vectors.rs, tests/vectors/jcs-vectors.json"
review_set: subset
---
# SR-002: Gap analysis — PLAT-1074 (quire-canonical#2)

## Summary

Ticket: PLAT-1074. The repo has no spec (`spec/` is empty), so this is a manual
acceptance-criteria-to-tests check. The criteria are Peter's ruling of
2026-09-26, the latest comment on PLAT-1074, which narrows the ticket to:
(1) raise `MAX_DEPTH` to at least 576; (2) in default mode, refuse any integer
above 2^53 in magnitude with a typed error; (3) no second profile and no
exact-integer mode. The ticket description names i64/u64 and i128/u128 as the
integer types in play.

| Criterion | Code | Tests | Status |
|---|---|---|---|
| AC-1 `MAX_DEPTH` ≥ 576 | src/lib.rs:83 | tests/limits.rs:165 (576 ok, 577 refused, typed `NestingDepth`); 1 MiB-stack test at `MAX_DEPTH` | covered; mutants 575 and 1024 killed |
| AC-2 i8–i64, u8–u64 above 2^53 refused | src/encoder.rs:156 via `integer()` | tests/encode.rs:102 (±2^53 accepted, ±(2^53+1) refused naming the value, `i64::MAX`, `u64::MAX`) | covered; bound mutants killed |
| AC-2 u128 | src/encoder.rs:410 | tests/encode.rs:125 (`1<<100`, `u128::MAX`) | covered; bypass mutant killed |
| AC-2 i128 | src/encoder.rs:390 via `integer()` | none | **gap** — FND-001 |
| AC-2 `serde_json::Value` integers (PosInt/NegInt) | same paths | tests/encode.rs:147 | covered |
| AC-2 integers in JSON text beyond `u64` | reach the encoder as `f64` | tests/encode.rs:180 asserts floats pass | **partial** — SR-001 FND-001 |
| AC-3 no second profile / exact-integer mode | no feature, flag or `Limits` field added | by absence in the diff | covered |

## Verdict

**CONDITIONAL** — every criterion has code and all but one integer path has a
test that goes red under mutation. The `i128` path has none, and the
JSON-text-beyond-`u64` behaviour is recorded as SR-001 FND-001.

## Findings

| ID | Severity | Summary | Refs |
|---|---|---|---|
| FND-001 | medium | No test drives `serialize_i128` past the bound. Mutating it to `self.double(value as f64)` leaves the whole suite green (measured, `--no-fail-fast`), so a later refactor of the i128 arm could silently round `i128` values such as `-(1_i128 << 100)` or `i128::MIN`. The ticket names i128 explicitly. Add `to_vec(&(-(1_i128 << 60)), ..)` and `to_vec(&i128::MIN, ..)` asserting `IntegerMagnitudeAboveMaximum(value)`, plus `±(1_i128 << 53)` accepted. | src/encoder.rs:390, tests/encode.rs:102 |

## Dispositions

Disposition pass at `agent-ix/quire-canonical@2c135ece7f1ceee7f98c7cf95393c25cbd92e3eb`
(fix commit `2c135ec`). Re-measured: mutating `serialize_i128` to
`self.double(value as f64)` now fails `integers_past_two_pow_53_in_magnitude_are_refused`
at `tests/encode.rs:152`. The AC-2 "integers in JSON text beyond `u64`" row is
now documented and pinned (SR-001 FND-001 disposition).

| FND | Outcome | sha/reason |
|---|---|---|
| FND-001 | fixed | 2c135ec — `tests/encode.rs:144` adds i128 cases: `±(1_i128 << 53)` accepted, `-(1_i128 << 60)` and `i128::MIN` refused with `IntegerMagnitudeAboveMaximum(value)`; i128 bypass mutant killed. |
