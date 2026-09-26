---
id: SR-001
title: "Code review — PLAT-1074 MAX_DEPTH 576 and 2^53 integer refusal (quire-canonical#2)"
type: SpecReview
analysis: code-review
scope: "agent-ix/quire-canonical@e7353e227196b8f576910faac375595bcbe403b2; Cargo.toml, Cargo.lock, src/encoder.rs, src/error.rs, src/lib.rs, src/number.rs, tests/encode.rs, tests/limits.rs, tests/vectors/jcs-vectors.json"
review_set: subset
---
# SR-001: Code review — PLAT-1074 (quire-canonical#2)

## Summary

Ticket: PLAT-1074. Reviewed `git diff origin/main...HEAD` at
`e7353e2` (two commits: `fcafb68`, `e7353e2`), with a `rust-review` lane
folded in. The change raises `Limits::MAX_DEPTH` from 512 to 576, replaces the
exact-double integer rule with a flat `|n| <= 2^53` bound
(`safe_integer_double`), renames the error variants to
`IntegerMagnitudeAboveMaximum(i128)` / `UnsignedIntegerMagnitudeAboveMaximum(u128)`,
bumps the crate to 0.2.0, and drops `18014398509481984` (2^54) from the
`integers` golden vector.

Examined clean: `Cargo.toml`/`Cargo.lock` (version bump only; `Error` is
`#[non_exhaustive]` and the variant rename is a breaking change, so 0.1 → 0.2
is correct for a 0.x crate); `src/encoder.rs` (every `serialize_*` integer arm
routes through `integer()` or the u128 arm; integer map keys go through
`decimal()` and are written as exact decimal strings, which is the ruling's
"exact integers travel as strings"); `src/lib.rs` depth constant;
`tests/limits.rs` (576 accepted, 577 refused with the existing
`LimitKind::NestingDepth` error; the 1 MiB-stack test now runs at 576).
`arbitrary_precision` is not enabled anywhere (dev-dependency enables only
`float_roundtrip`; the optional dependency enables only `preserve_order`), and
its private token is already refused (`Error::SerdeJsonPrivateToken`).

Recomputed the edited golden vector independently: `node` `JSON.stringify` of
the new preimage prints
`[0,-1,9007199254740991,-9007199254740992,9007199254740992,1e+21]`, and
`sha256sum` of that text is
`029b5c7538fcc510b6f5e2e29516ea704f78a3d0ae1247a113debdefc7138695` — matches
the file. The old digest `cde2c731…` also reproduces from the old text.

Mutation checks (each applied, suite run with `--no-fail-fast`, reverted):

| Mutant | Result |
|---|---|
| bound `> 2^53` → `> 2^53 + 1` | killed (unit test + both new integration tests) |
| bound `>` → `>=` (refuse 2^53 itself) | killed |
| bound check removed | killed (unit + both integration tests) |
| u128 arm bypasses the check | killed (`tests/encode.rs:125`) |
| `MAX_DEPTH` 575 / 1024 | killed (`tests/limits.rs:169`) |
| `serialize_i128` bypasses the check | **survived** — see SR-002 FND-001 |

## Verdict

**CONDITIONAL** — the bound, the depth change and the recomputed vector are
correct and the new tests are strong oracles. One medium finding: integers in
JSON text that exceed `u64` still lose precision silently through the float
path, and the `integers` golden vector keeps one such value as an accepted
integer, contradicting the crate's own new documentation. Two low findings.

## Findings

| ID | Severity | Summary | Refs |
|---|---|---|---|
| FND-001 | medium | Integral values above 2^53 that reach the encoder as `f64` are encoded, not refused, and nothing documents it. `serde_json` parses an integer literal outside `i64`/`u64` range as `Number::Float`, so `serde_json::from_str("100000000000000000001")` encodes as `100000000000000000000` and `"18446744073709551617"` as `18446744073709552000` — silent precision loss, measured. The `integers` golden vector still lists `1000000000000000000000` as an accepted integer, while `src/lib.rs` now says an integer is refused if its magnitude exceeds 2^53, and `tests/encode.rs:186` says JSON-text values take "the identical `serde_json::Number` path" (true only inside `u64`/`i64`). Refusing all integral doubles above 2^53 is not proposed (RFC 8785 allows `1e+21`); fix by stating the boundary in `lib.rs`/`number.rs` docs (the bound applies to Rust integer types; a JSON-text integer beyond `u64` is already a double when the encoder sees it), moving `1e21` out of the `integers` vector into `floats`, and pinning the behaviour with a test. | src/lib.rs:15, tests/vectors/jcs-vectors.json:84, tests/encode.rs:180, tests/encode.rs:186 |
| FND-002 | low | The golden vector file gained no refusal entry. Removing 2^54 was necessary (the harness only has an accept path for `vectors`), but the file now carries no integer past the bound at all, so the refusal lives only in Rust tests. A `refused_integers` section (e.g. `9007199254740993`, `-9007199254740993`, `18014398509481984`, `9223372036854775807`, `-9223372036854775808` as JSON text) asserted to `IntegerMagnitudeAboveMaximum` would keep the rule in the data corpus next to `refused_numbers`. Only this repo reads the file today, hence low. | tests/vectors/jcs-vectors.json:84, tests/vectors.rs:150 |
| FND-003 | low | Doc and message inaccuracies around the bound. `number.rs:20` says 2^53 is "the largest integer magnitude that is exactly a double" — false (2^60 and 2^100 are exact; the module doc two lines up says so). `error.rs:106` reads "the largest magnitude every RFC 8785 number encodes exactly", which does not parse. The name `MAX_SAFE_MAGNITUDE` and the message "maximum safe magnitude of 2^53" collide with ECMAScript's `Number.MAX_SAFE_INTEGER` = 2^53 − 1; a TS implementer porting the rule from the message would refuse 2^53, which this crate accepts (the inclusive bound matches Peter's ruling, only the word "safe" misleads). | src/number.rs:20, src/error.rs:106, src/error.rs:109 |

## Gates Run

| Gate | Result |
|---|---|
| `make ci` (fmt-check, clippy default + `--all-features`, test default + `test-preserve-order`, deny, audit-unsafe, docs) | pass, `exit=0`, head `e7353e2`; reviewer log `rv-canonical-2-ci.log` |
| Coder log `plat-1074-ci.log` | first line `head=e7353e227196b8f576910faac375595bcbe403b2`, last line `exit=0` |
| Golden vector recompute (`node` + `sha256sum`) | matches |
| Mutation checks | 7 of 8 killed; `serialize_i128` survived |

## Language Dispatch

Rust only (`Cargo.toml`, `src/*.rs`, `tests/*.rs`) plus one JSON fixture;
`rust-review` ran as a lane of this file. Repo idioms from `CLAUDE.md`
(`make ci` as the gate, `// SAFETY:` audit, rustfmt 100 cols, clippy
`-D warnings`) were followed. New tests use the repo's existing
`/// PLAT-NNN:` doc-comment tracing style. No new `#[allow]` without a reason;
the one kept `clippy::cast_precision_loss` allow carries its reason. No
`unsafe`, no panics on the library path, no CI workflow change in the diff.

## Dispositions

Disposition pass at `agent-ix/quire-canonical@2c135ece7f1ceee7f98c7cf95393c25cbd92e3eb`
(fix commit `2c135ec`, the only commit after `e7353e2`). Each outcome was
re-checked against the code, not taken from the commit message. `make ci` at
`2c135ec`: exit 0 (reviewer log `rv-canonical-2b-ci.log`). The new `integers`
digest `ffab7194a2b81133c50a3fd11587099b5c88d93df98ef5fe46799dd9f3badf0d`
reproduces from `node` `JSON.stringify` + `sha256sum`. Removing the bound check
now also fails `tests/vectors.rs:186` (the new `refused_integers` gate).

| FND | Outcome | sha/reason |
|---|---|---|
| FND-001 | fixed | 2c135ec — boundary documented in `src/lib.rs:14` and `src/number.rs:18`; behaviour pinned by `json_text_integers_beyond_u64_take_the_float_path_and_are_not_refused` (`tests/encode.rs:223`); `1e21` removed from the `integers` vector (already covered by the `floats` vector and `exponent-threshold-1e21`). JSON text beyond `u64` still rounds by design; that is now stated, not hidden. |
| FND-002 | fixed | 2c135ec — `refused_integers` section (5 entries) in `tests/vectors/jcs-vectors.json:123`, asserted to `IntegerMagnitudeAboveMaximum` by `tests/vectors.rs:178`; goes red when the bound check is removed. |
| FND-003 | fixed | 2c135ec — `MAX_SAFE_MAGNITUDE` → `MAX_EXACT_INTEGER_MAGNITUDE`, `safe_integer_double` → `exact_integer_double`; doc now states "every smaller magnitude, and this one, holds exactly" (true: 2^53+1 is the first non-exact integer) and names the ES `MAX_SAFE_INTEGER` = 2^53 − 1 difference; `error.rs` doc rewritten and message says "maximum integer magnitude". |
