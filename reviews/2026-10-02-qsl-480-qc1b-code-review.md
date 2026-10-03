---
id: SR-1262
title: "Code review of quire-canonical PR #7: Encode for serde_json::Value and drop_value (QSL-480 slice QC1b)"
type: SpecReview
analysis: code-review
scope: "agent-ix/quire-canonical@4734d3f7badccc13678f756bdd5179716cedb1db; PR #7 diff against origin/main (59fe4f0): CLAUDE.md, Cargo.toml, Makefile, README.md, src/{error,lib,value,writer}.rs, tests/value.rs"
review_set: subset
---
# Code review of quire-canonical PR #7

## Summary

Ticket: QSL-480 (slice QC1b). PR: quire-canonical#7, branch
task/qc1b-value-encode. The PR adds a `serde_json` feature, off by default,
that implements `Encode` for `serde_json::Value` with an explicit heap stack.
It also adds the public `drop_value`, a new `Error::WideIntegerMagnitudeAboveMaximum`
behind the feature, and a `pub(crate) Writer::refuse`. The rust-review lane
is folded into this file.

What the brief asked to check:

- **Iterative walk.** Verified. `encode_into` keeps one `Open` entry per open
  array or object (src/value.rs:50-112). It pushes the entry only after
  `begin_array`/`begin_object` succeeded, so every entry stands for a counted
  byte. Nothing recurses and nothing caps depth. I traced the event order for
  scalars at the top level, empty and nested containers, and object members
  (`name` before each value).
- **Key order.** Members go to the `Writer` in map iteration order, and the
  `Writer` sorts each object once when it closes (src/writer.rs:584).
  value.rs does not sort. `ordinary_values_match_the_serde_path_and_their_canonical_text`
  uses U+10000 against U+E000, where UTF-8 order (what a `BTreeMap` iterates)
  and UTF-16 order differ. It runs against the `BTreeMap` map in test-default
  and the insertion-ordered map in test-preserve-order.
- **Numbers.** `as_i64`/`as_u64` go to `Writer::integer(i128)` and the
  existing 2^53 refusal. Floats go to `Writer::number` and the existing
  ECMAScript text and non-finite rules.
- **`Writer::refuse`.** It is new, `pub(crate)`, gated on the feature, and
  only wraps the unchanged `guard`. No existing caller uses it, so behaviour
  for existing callers is unchanged. It marks the writer refused. A caller
  driving `encode_into` on its own `Writer` then gets `AfterRefusal` for any
  later event, as the `Writer` docs promise. See FND-004 for its gate.
- **Allocation failure.** The walk stack grows with `try_reserve`, and a
  failure becomes `Error::Allocation` through `refuse`, with no panic or
  abort (src/value.rs:80-84). The `drop_value` docs say that running out of
  memory aborts (src/value.rs:126-128). The only infallible allocation inside
  `Encode` is `number.to_string()` (src/value.rs:169). It is reachable only
  under serde_json's `arbitrary_precision`, and its size is bounded by the
  literal's length. That matches the existing `DuplicateMemberName` path.
- **Feature-gated `Error` variant.** `Error` is `#[non_exhaustive]`
  (src/error.rs:105), so a downstream match already needs a wildcard arm and
  stays exhaustive whatever the feature set. `Error::Sink` is the precedent
  for a gated variant. The gate on this one has no reason, though (FND-003).
- **`ViaSerde` with a literal `DEPTH = 0`.** It lives only in
  tests/value.rs:22-36. `DEPTH` is evaluated only at compile time
  (`const { T::DEPTH }`, src/encoder.rs:34) and never compared at run time,
  so the literal changes nothing at run time. The wrapped values are at most
  three levels deep. It cannot hide a depth bug in the comparison: the
  `Value` side never goes through serde, and every case also asserts an
  independent literal text.
- **Test oracles.** The deep tests assert the whole 200,000-byte text, not
  just success. The ordinary cases assert literal canonical text and compare
  with the serde path. The golden vectors assert canonical bytes and SHA-256
  taken from the vector file. The refusals assert the variant and its
  payload. There are two weak spots: `drop_value` (FND-005) and the
  `arbitrary_precision` branch (SR-1263 FND-002).
- **The coder's claims.** I checked these against the logs and the tree:
  - `make ci` ran at this head and exited 0. The log was written at 21:19,
    after the 21:18:45 commit, and the coder worktree is at 4734d3f with a
    clean tree. tests/value.rs ran 7 tests in test-default and 7 in
    test-preserve-order, and 0 in test-no-std. Both thumbv7em builds ran.
  - The no_std clippy with the feature and the `arbitrary_precision` run were
    on 141fe8e. `encode_into` and `encode_number` have not changed since
    141fe8e: only `drop_value` and docs were added. So that run still covers
    the encode code.
- **Rust lane.** The crate forbids `unsafe` and the PR adds none. There is
  no `unwrap` or indexing on a production path; `unwrap_or` falls back to a
  refusal. Imports are grouped std, external, crate. Every new public item
  has docs, and the code fits MSRV 1.81.
- **Ceremony.** There is no ceremony, compatibility layer, vendored file or
  recorded version. The SHA-256 values in the tests are the crate's own
  identity digests, which are the function under test.
- **Gates.** Not re-run. I read the coder's make ci log.

## Verdict

Changes requested: two medium and four low findings. FND-001 is a
one-line Cargo feature. FND-002 is a doc correction, plus a policy choice for
the ticket owner. The rest are small code or test changes. The walk, the key
order, the integer and float routing, and `drop_value` itself are correct.

## Findings

| ID | Severity | Summary | Refs |
| --- | --- | --- | --- |
| FND-001 | medium | The library's optional serde_json is built without `float_roundtrip`. Without that feature, serde_json parses floats with best-effort precision: in f64_from_parts it converts the u64 significand to f64 and then multiplies or divides by a power of ten, which rounds twice. So the same JSON text gives different canonical bytes through `Value` than through `read`, and different bytes again in builds that do enable `float_roundtrip`. Traced example: `9007199254740993.0` parses to 90071992547409930 / 10, which is 9007199254740994.0 and encodes as `9007199254740994`. `read` (Rust's correctly rounded `parse::<f64>`) gives `9007199254740992`. quire-contract-ir enables only `unbounded_depth`, and quire-contract-codegen uses serde_json's default features, so neither consumer has `float_roundtrip` today. The suite cannot show this: the dev-dependency turns `float_roundtrip` on (Cargo.toml:50-52), and that is why `every_golden_vector_preimage_encodes_as_a_value` passes. Fix: add `"float_roundtrip"` to the optional dependency's features (Cargo.toml:46). Feature unification then makes serde_json parse floats correctly in every build that compiles the `Value` impl. Say so in the README paragraph. | Cargo.toml:46; Cargo.toml:50-52; README.md:47-54; tests/value.rs:113-130 |
| FND-002 | medium | The doc says that with `arbitrary_precision` "the bytes and refusals are the same as without it". That is false for a JSON integer literal past 64 bits. Without the feature, serde_json parses `18446744073709551617` as the double 1.8446744073709552e19, which encodes as `18446744073709552000`. With it, the literal stays text, and `encode_number` refuses it with `WideIntegerMagnitudeAboveMaximum`. Feature unification turns `arbitrary_precision` on for every crate in a build once any crate enables it; engineering-assurance's `full` feature does. So whether a body containing such a literal encodes or is refused depends on unrelated crates in the build. The claim "Two texts exist only in that mode" is also misleading: the literal exists in both modes, and only its representation differs. Fix: state the dependence on the build accurately in src/value.rs and the README. Or, if the owner wants mode-independent results, send a wide integer literal through the double path under `arbitrary_precision`, the same as without it. That trades against refusing a genuine `Number::from_u128` past 64 bits, so it is a ticket-owner call. | src/value.rs:37-42; src/value.rs:167-176 |
| FND-003 | low | `Error::WideIntegerMagnitudeAboveMaximum` is gated on `#[cfg(feature = "serde_json")]` for no reason. Its payload is an `alloc` `String`, unlike `Error::Sink`, whose `io::Error` needs `std`. The gate means the variant set changes with a feature. It also means a downstream crate that names the variant compiles only if quire-canonical's `serde_json` feature is on, which it must enable itself rather than rely on feature unification. Fix: drop the `cfg` and say in the doc that only the `serde_json` feature produces it. | src/error.rs:124-129 |
| FND-004 | low | `Writer::refuse` is gated on the `serde_json` feature, but the need is general: any `Encode` impl that finds its own fault must mark the writer refused. The crate's other heap walk, `NodeRef::encode_into`, returns `Error::Allocation` from `reserve_tasks` without marking the writer refused. So the two walks handle the same failure differently, and only one keeps the documented "after any refusal the writer refuses every further event". Failure scenario: a caller calls `writer.begin_array()`, ignores an `Allocation` error from `node.encode_into(&mut writer)` raised at the first `push_task`, then calls `writer.end_array()` and `writer.finish()`. It gets `Ok` with `[]`. Fix: remove the `cfg` on `refuse` and route the `reserve_tasks` failure in read.rs through it. | src/writer.rs:399-404; src/read.rs:414-426 |
| FND-005 | low | `drop_value_drops_100000_deep_values_on_a_64_kib_stack` checks only that the drops return without overflowing. A `drop_value` whose body is `core::mem::forget(value)` passes it, and so do the deep encode tests, so a leak in `drop_value` goes unseen. Fix: add one test in a binary of its own with a counting global allocator, the pattern tests/memory.rs uses. It should check that live heap returns to its baseline after `drop_value` on a 100,000-deep array and object. | tests/value.rs:192-202; src/value.rs:139-148 |
| FND-006 | low | The `expect("no stack overflow")` in `on_small_stack` is misleading. `join()` returns `Err` only for a panic; a stack overflow aborts the whole process. Also, if `to_vec` refuses inside `encode_on_small_stack`, the `expect` panics while the deep `Value` is alive. Unwinding then drops the value with serde_json's recursive `Drop` on the 64 KiB stack. The binary aborts and the rest of its tests are never reported. Fix: return the `Result` out of the closure, call `drop_value(value)` before propagating it, and rename the message (for example, "small-stack worker panicked"). | tests/value.rs:134-141; tests/value.rs:166-174 |

## Dispositions

Round 1, reviewed at 1c714f52af3f934de56b53a9cec93c0b88176976. I read the
diff 4734d3f..1c714f5 commit by commit, and checked the coder's logs against
the tree. `make ci` ran at this head and exited 0; the coder worktree is at
1c714f5 and clean. The log shows test-default, test-preserve-order,
test-no-std, the new test-arbitrary-precision lane, both thumbv7em builds,
deny, audit-unsafe and docs. The mutation log removes `float_roundtrip` and
the new float test then fails with `9007199254740994` against
`9007199254740992`, which confirms the FND-001 example. I did not re-run any
build.

| FND | Outcome | sha/reason |
| --- | --- | --- |
| FND-001 | fixed | 6ef63b4: the optional serde_json now has `features = ["alloc", "float_roundtrip"]` (Cargo.toml:50-53). The Cargo.toml comment and README.md:50-54 say why. `float_literals_give_the_same_bytes_through_a_value_as_through_read` (tests/value.rs:147) checks five literals through both `Value` and `read`. The test cannot catch the feature being removed from the library dependency alone, because the dev-dependency also turns it on; the Cargo.toml comment is the guard. That limit is inherent, so it is not a new finding. |
| FND-002 | fixed | 71925c3, eb7ff0b: src/value.rs:33-54 no longer claims the two modes match. It says a literal past the 64-bit range encodes as its double without `arbitrary_precision` and is refused with it, and that the result therefore depends on the build. README.md:58-67 says the same. The owner-level alternative (mode-independent results) was not taken, which the finding allowed. |
| FND-003 | fixed | 4c96b85: the `cfg` is gone from `Error::WideIntegerMagnitudeAboveMaximum`. Its doc says only the `serde_json` feature's `Value` encoder produces it (src/error.rs:124-129). |
| FND-004 | fixed | 4c5b611: `Writer::refuse` is no longer gated (src/writer.rs:399-403). `NodeRef::encode_into` reserves every task through `reserve_tasks(writer, ...)`, which refuses through the writer (src/read.rs:375-376, 419-432). `push_task` is gone, so no path returns `Allocation` without marking the writer refused. |
| FND-005 | fixed | 6ec9e19, 88a4b8e, 5044b09: tests/drop_value.rs is a one-test binary with a counting global allocator. It checks that the value held at least `DEEP * size_of::<Value>()` bytes, so the counter is live and the check is not vacuous. It then checks that live heap after `drop_value` equals the baseline, for a 100,000-deep array (with a string and an object at the leaf) and a 100,000-deep object. A warm-up pass absorbs one-time allocations, and the asserts run after the drop. `mem::forget` would fail it. It ran in test-default and test-preserve-order. |
| FND-006 | fixed | 514a3b8: the message is now "small-stack worker panicked", and the doc says an overflow aborts the process (tests/value.rs:161-170). `encode_on_small_stack` returns the `Result` out of the thread after `drop_value`, and unwraps it on the caller's stack (tests/value.rs:197-205). |
