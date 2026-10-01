---
id: SR-003
title: "Code review — quire-canonical no_std + alloc (quire-canonical#5)"
type: SpecReview
analysis: code-review
scope: "agent-ix/quire-canonical@6141cbc19e3bd6d551991e0a53d67edff4ebc422; Cargo.toml, Makefile, clippy.toml, rust-toolchain.toml, README.md, CLAUDE.md, src/encoder.rs, src/error.rs, src/escape.rs, src/identity.rs, src/lib.rs, src/number.rs, src/order.rs, src/sink.rs, tests/alloc_surface.rs, tests/encode.rs, tests/vectors.rs"
review_set: subset
---
# SR-003: Code review — quire-canonical no_std + alloc (quire-canonical#5)

## Summary

Ticket: QSL-358 (consumer ticket; the branch `task/no-std-alloc` names none).
Diff: `git diff 1d73f93...6141cbc`, 17 files, +171/-28. Includes the
`rust-review` lane.

The crate becomes `#![no_std]` + `extern crate alloc`, with a default `std`
feature that forwards `serde/std` and `sha2/std`. Only `WriteSink` and
`Error::Sink(#[from] io::Error)` are gated. `std::error::Error` becomes
`core::error::Error`, and `rust-version` and clippy `msrv` go from 1.80 to 1.81.
The Makefile adds `test-no-std` (host `cargo test --no-default-features`),
`build-no-std` (thumbv7em-none-eabi), and clippy for both feature sets, all in
`make ci`.

Checked:

- **Byte identity.** `tests/vectors/jcs-vectors.json` has no diff
  (`git diff 1d73f93...HEAD -- tests/vectors` is empty). `tests/vectors.rs`
  only gates the `WriteSink` block, and no assertion changed. The vector tests
  (8) pass in all three lanes: default, `test-preserve-order` and
  `--no-default-features`.
- **Floats.** `ryu-js` 1.0.3 is unconditionally `#![no_std]` and has no `std`
  feature (only `small`), so both builds run the same formatter code.
  `number.rs` uses only `f64::is_finite` (core). Nothing in the float path
  depends on `std`.
- **Feature hygiene.** `Error` was already `#[non_exhaustive]` at the base
  (`src/error.rs:96`), so gating `Sink` cannot break a downstream exhaustive
  match. `From<io::Error>` stays under default features. `core::error::Error`
  is the same trait `std::error::Error` re-exports. In QSL `origin/main`, the
  only use of an io path is `qsl-semantics/src/model/key.rs:535`
  (`WriteSink(std::io::sink())`), through a default-features workspace dep.
  IR has no reference, and CG has only a transitive lockfile and `deny.toml`
  entry. `cargo check --all-targets -p qsl-semantics -p qsl-package -p qsl-eval
  -p qsl-replay` on a QSL `origin/main` worktree, with this branch patched in,
  passed.
- **Real no_std.** `cargo build --no-default-features --target
  thumbv7em-none-eabi` passed from a fresh target dir. thumbv7em-none-eabi
  ships no `std` crate, so any `std` reference would fail to compile.
  `cargo tree --no-default-features --target thumbv7em-none-eabi -e features
  -e normal` shows no `std` feature on any dependency (only `serde`/`serde_core`
  `alloc`). Resolver 2 keeps dev-dependency features out of this build.
- **MSRV.** 1.81 is the release that stabilised `core::error::Error`, so the
  bump is required and minimal. Both feature sets build on 1.82 with
  `--locked` (no 1.81 toolchain is installed locally). RT's `rust-version` is
  1.98.1 and quire-exact's is 1.98, so the bump constrains no consumer.
- **Workflows.** `git diff 1d73f93...HEAD -- .github` is empty.
- `cargo doc --no-default-features` with `-D warnings` is clean: no
  intra-doc link to a gated item.

## Verdict

The PR is sound: a correct, minimal no_std conversion. It is byte-identical,
no consumer breaks, the no_std build really proves it, and the MSRV bump is
necessary. There is one low finding, a doc overclaim in the new test file.
Mergeable as is, and FND-001 can be fixed in passing.

Not a finding, but a residual gap: tests run only on the x86_64 host, so
byte identity on a 32-bit no_std target is argued, not executed. `ryu-js` and
the integer code are pure, target-independent arithmetic. Recorded for QSL
and RT, who own the embedded lanes.

`.github/workflows/ci.yml` (manual dispatch only) does not run the no-std
lanes. The workflows are untouched as required, and local `make ci` is the
gate.

## Findings

| ID | Severity | Summary | Refs |
| --- | --- | --- | --- |
| FND-001 | low | The `tests/alloc_surface.rs` module doc says the file exercises the crate "with nothing from `std`" under `make test-no-std`. That lane is a host `cargo test --no-default-features`. The test harness links `std`, and the dev-dependencies (`serde` with default features, `serde_json`) unify `serde/std` into the library under test, as `cargo tree --no-default-features -e features` shows. So the lane proves only the crate's own `std`-off cfg and the alloc API surface. The proof that nothing pulls in `std` is `build-no-std`. Reword the doc to say so. | tests/alloc_surface.rs:4, tests/alloc_surface.rs:6 |

## Gates Run

- `cargo build --no-default-features --target thumbv7em-none-eabi` from a
  fresh target dir (`target/review`): exit 0.
- `cargo test`, `cargo test --features test-preserve-order`,
  `cargo test --no-default-features` (CARGO_BUILD_JOBS=4): all pass. The
  no-default lane runs 16 encode tests, because the `WriteSink` test is gated.
- `cargo +1.82 build --lib --locked`, with and without default features: exit 0.
- `RUSTDOCFLAGS=-D warnings cargo doc --no-deps --no-default-features`: exit 0.
- QSL consumer check (see Summary): exit 0.
- Coder's `make ci` log reviewed: lint for both feature sets plus the
  thumbv7em lib, three test lanes, build-no-std, deny, unsafe audit, docs.

## Language Dispatch

Rust: the `rust-review` lane is folded in. No `unsafe` (`#![forbid(unsafe_code)]`
stays). No new panics in library code. No new integer conversions. The test
files' `std::` imports inside `#[cfg(test)]` are covered by
`#[cfg(any(feature = "std", test))] extern crate std`. The hard-coded SHA-256 in
`alloc_surface.rs` is a behaviour assertion on the crate's identity-digest
feature, computed independently. It is not a file or version pin.

## Dispositions

Round 1, reviewed at `3d5961a59bc4a8baefaf63d793f9d579bcea68b0`.

| FND | Outcome | sha/reason |
| --- | --- | --- |
| FND-001 | fixed | 3d5961a. The `tests/alloc_surface.rs` header now says the host `--no-default-features` lane exercises only the std-off code paths. It says the harness links `std` and the dev-deps turn on `serde/std`, and names `make build-no-std` (thumbv7em-none-eabi) as the proof that nothing from `std` is linked. The change is doc-only: the alloc_surface tests pass under `--no-default-features` and `cargo fmt --check` is clean. |
