---
id: SR-1263
title: "QSL-480 gap analysis of quire-canonical PR #7 (slice QC1b) against the task requirements and the crate's README and AGENTS.md contract"
type: SpecReview
analysis: gap-analysis
scope: "agent-ix/quire-canonical@4734d3f7badccc13678f756bdd5179716cedb1db; PR #7 diff against origin/main (59fe4f0); contract: the QC1b task requirements (R1-R5 in the reviewer brief), README.md, AGENTS.md, CLAUDE.md and the crate docs in src/lib.rs. spec/ holds only .gitkeep, so there is no spec artifact to trace to."
review_set: subset
---
# QSL-480 gap analysis of quire-canonical PR #7

## Summary

Ticket: QSL-480 (slice QC1b). PR: quire-canonical#7. quire-canonical has no
spec directory, so the contract is the five task requirements plus the
crate's own README, AGENTS.md, CLAUDE.md and crate docs.

Requirement to code and test trace:

- **R1, the `serde_json` feature.** Covered. `serde_json = ["dep:serde_json"]`
  is not in `default`. The optional dependency is
  `default-features = false, features = ["alloc"]` (Cargo.toml:30, 46). The
  make ci log at this head shows both
  `cargo build --no-default-features --target thumbv7em-none-eabi` and the
  same build with `--features serde_json`, and both pass.
- **R2a, iterative with no depth cap.** Covered. There is one heap `Open`
  entry per open container and no recursion or depth check
  (src/value.rs:50-112). Tests:
  `a_100000_deep_array_encodes_on_a_64_kib_stack` and
  `a_100000_deep_object_encodes_on_a_64_kib_stack`.
- **R2b, UTF-16 key order.** Covered. Only the `Writer` sorts, so nothing is
  sorted twice, and nothing relies on map order.
  `ordinary_values_match_the_serde_path_and_their_canonical_text` asserts the
  U+10000 / U+E000 / U+E9 / `z` order under both the `BTreeMap` lane and the
  `preserve_order` lane.
- **R2c, integers through `Writer::integer` with the 2^53 refusal.**
  Covered. `integers_past_two_pow_53_are_refused` checks 2^53+1, -(2^53+1),
  `u64::MAX` and `i64::MIN`, at the top level and nested, with exact
  payloads. The ordinary cases cover ±2^53 and 2^53-1. See FND-001 for how
  this rule sits against the crate's rule for JSON text.
- **R2d, floats through the number rules.** Covered for finite values (the
  ECMAScript cases and the golden vectors). Non-finite numbers reach the
  encoder only under `arbitrary_precision`, and no make lane runs that
  branch (FND-002).
- **R3a, 100k-deep array and object on a small stack.** Covered on a 64 KiB
  stack, asserting the exact 200,000-byte text.
- **R3b, byte identity with the serde path.** Covered for ten ordinary
  values through `ViaSerde`. `every_golden_vector_preimage_encodes_as_a_value`
  adds the vectors' independent bytes and digests. (Its pass depends on the
  dev-dependency's `float_roundtrip`; see SR-1262 FND-001.)
- **R3c, refusals.** Integer refusals are covered. The
  `arbitrary_precision` refusals have no make lane (FND-002). There is no
  byte-limit refusal through the `Value` walk (FND-003).
- **R4, `drop_value`.** Public (src/lib.rs re-export), with no recursion and
  no depth cap. `drop_value_drops_100000_deep_values_on_a_64_kib_stack`
  shows it does not recurse. Whether it frees memory is untested (SR-1262
  FND-005).
- **R5, Makefile lanes.** Covered. `test-default` runs
  `cargo test --features serde_json`, and `build-no-std` builds with and
  without the feature (Makefile:53-55, 67-70). `test-preserve-order` also
  turns the feature on.
- **AGENTS.md / CLAUDE.md.** The PR adds no hash, pin, digest catalog or
  version record. The layout and command lists in CLAUDE.md are updated.
- **README contract.** The new README paragraph matches the code, except as
  FND-001 and SR-1262 FND-001/FND-002 describe.

GitHub CI runs plain `cargo test` (.github/workflows/ci.yml:28), so it runs
none of the `Value` tests. That was already true of the preserve_order and
no-std lanes. `make ci` is the gate, so this is not a finding.

## Verdict

Changes requested: two medium and one low gap. FND-001 needs the ticket
owner to say which rule a `Value`'s integers follow. Whichever rule is chosen
must then be stated in the docs and pinned by a test. FND-002 is one
Makefile line. FND-003 is one test.

## Findings

| ID | Severity | Summary | Refs |
| --- | --- | --- | --- |
| FND-001 | medium | The `Value` path and `read` give different outcomes for the same JSON text, and no doc or test says this is intended. The crate's rule is that a JSON number is the double its text denotes, and only a Rust integer past 2^53 is refused (src/lib.rs:49-51; `json_text_integers_encode_as_their_double`, tests/read.rs:136-152). As R2c asks, the `Value` path refuses every integer serde_json holds as `u64`/`i64` past 2^53. So `serde_json::from_str::<Value>("9007199254740993")` is refused, while `read` encodes the same text as `9007199254740992`. The refusal also has a cliff. In a default serde_json build, `18446744073709551617` is past 64 bits and parses to a double, which encodes as `18446744073709552000`. So a larger literal is accepted and a smaller one refused: the same cliff SR-1240 recorded for the pre-QC1 encoder. IR and CG will digest bodies through this path, and any component that digests the same text through `read` will disagree on these inputs. Fix: the ticket owner rules on one of two options. (a) Keep the R2c refusal: say in the README and src/value.rs that it differs from `read` and has a cliff at 2^64. (b) Encode a `Value`'s integers as their double, as `read` does. Either way, add a test that parses one literal in (2^53, 2^64) and one past 2^64 and asserts the chosen outcome. | src/value.rs:33-34; src/value.rs:156-161; README.md:47-54; src/lib.rs:43-51; tests/read.rs:136-152 |
| FND-002 | medium | The `arbitrary_precision` code in `encode_number` runs in no make lane: the wide-integer refusal and the non-finite refusal (src/value.rs:167-176). Under `make test`, `numbers_only_arbitrary_precision_holds_are_refused` takes its `else` branch, which asserts only serde_json facts (`1e400` fails to parse, `from_f64` rejects infinity and NaN) and runs no crate code. The one run that tested the branch was a one-off on 141fe8e. The branch is reachable in real builds: engineering-assurance's `full` feature turns on `serde_json/arbitrary_precision`, and feature unification applies it to every crate in that build. This is the only coverage of R2d's non-finite refusal and of `WideIntegerMagnitudeAboveMaximum`. Fix: add `$(CARGO) test --features serde_json,serde_json/arbitrary_precision --test value` to `make test`. It is the command the coder already ran, and it passed on the same encode code. | Makefile:47-55; tests/value.rs:226-253; src/value.rs:167-176 |
| FND-003 | low | No test refuses a `Value` on the byte ceiling. The impl's doc says the walk stack is bounded by `Limits::max_bytes` because each entry follows a counted `[` or `{` (src/value.rs:24-28). The README says a limit refuses and never truncates. For `Document` this is pinned by `deep_document_encoding_is_refused_on_bytes`, but not for `Value`. Mutation it would catch: `let _ = writer.begin_array();` in place of `writer.begin_array()?;`. The current tests pass with it, and a limit refusal would then surface as `Protocol(AfterRefusal)` rather than `Limit`. Fix: encode a 5,000-deep `Value` array under `Limits::new(4_999)` and assert `Error::Limit(LimitExceeded { kind: CanonicalBytes, bound: 4_999, required: 5_000 })`, mirroring tests/limits.rs:142-158. | src/value.rs:24-28; src/value.rs:70-77; tests/limits.rs:139-158 |

## Dispositions

Round 1, reviewed at 1c714f52af3f934de56b53a9cec93c0b88176976. I read the
diff 4734d3f..1c714f5, and the make ci log at this head, which exited 0. All
three new tests passed in every lane that runs tests/value.rs: test-default,
test-preserve-order and test-arbitrary-precision. I did not re-run any build.

| FND | Outcome | sha/reason |
| --- | --- | --- |
| FND-001 | fixed | eb7ff0b: option (a) was taken, keeping R2c's refusal and documenting it. README.md:58-67, src/value.rs:33-41 and src/lib.rs:49-54 say a `Value`'s integers follow the integer rule, not `read`'s rule for JSON text. They give the `9007199254740993` example, and say a literal past 64 bits encodes as its double unless `arbitrary_precision` is on. `integer_literals_follow_the_integer_rule_only_within_64_bits` (tests/value.rs:286) pins all of it. Literals in (2^53, 2^64] (including `u64::MAX` and `i64::MIN`) are refused with exact payloads while `read` gives the double. Past 64 bits, the test checks the double without `arbitrary_precision` and `WideIntegerMagnitudeAboveMaximum` with it, and the `read` double in both modes. |
| FND-002 | fixed | 5b980cc: the new `test-arbitrary-precision` target runs `cargo test --features serde_json,serde_json/arbitrary_precision --test value`, and `test` depends on it (Makefile:48, 67-72). It ran in make ci at this head, 10 passed. No workflow file changed. |
| FND-003 | fixed | 1c714f5: `deep_value_encoding_is_refused_on_bytes` (tests/value.rs:239) encodes a 5,000-deep array under `Limits::new(4_999)` and asserts `Limit { kind: CanonicalBytes, bound: 4_999, required: 5_000 }`. It also checks that the same value fits in 10,000 bytes, and calls `drop_value` before asserting. The `let _ = writer.begin_array();` mutation would now fail with `Protocol(AfterRefusal)`. |
