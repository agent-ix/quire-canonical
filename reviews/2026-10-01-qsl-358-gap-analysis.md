---
id: SR-004
title: "Gap analysis — quire-canonical no_std + alloc (quire-canonical#5)"
type: SpecReview
analysis: gap-analysis
scope: "agent-ix/quire-canonical@6141cbc19e3bd6d551991e0a53d67edff4ebc422; Cargo.toml, Makefile, src/lib.rs, src/error.rs, src/sink.rs, tests/alloc_surface.rs, tests/encode.rs, tests/vectors.rs, tests/vectors/jcs-vectors.json"
review_set: subset
---
# SR-004: Gap analysis — quire-canonical no_std + alloc (quire-canonical#5)

## Summary

Ticket: QSL-358 (consumer). The repo has no spec (`spec/` is empty), so this
is a manual check of acceptance criteria against tests. The criteria come from
the dispatching brief. quire-canonical is the single JCS encoder (ADR-013 §2),
and QSL's no_std leaf `quire-semantic-value` and RT must mint compound-unit ids
through it. The rule is no second encoder and no shim.

| Criterion | Code | Tests / evidence | Status |
|---|---|---|---|
| AC-1 Crate builds as no_std + alloc | src/lib.rs:55-61, Cargo.toml features | `make build-no-std` (thumbv7em-none-eabi); reviewer re-ran it from a fresh target dir; `cargo tree` shows no `std` feature in the normal graph | covered |
| AC-2 Existing std users unaffected | default = ["std"]; `WriteSink` and `Error::Sink` behind `std`; `Error` already `#[non_exhaustive]` | default and preserve-order test lanes are unchanged and pass; QSL `origin/main` consumers (`qsl-semantics`, `qsl-package`, `qsl-eval`, `qsl-replay`) pass `cargo check --all-targets` against this branch | covered |
| AC-3 Encodings byte-identical in both feature sets | no encoder logic changed (only `std::` to `core::`/`alloc::` paths) | `tests/vectors.rs` golden vectors pass under `--no-default-features`; the vector file has no diff | covered (host only; see Verdict) |
| AC-4 A compound-unit-shaped preimage mints without std | `to_vec`, `sha256`, `encode` ungated | `tests/alloc_surface.rs` checks literal bytes and an independently computed SHA-256; the tee path and a caller-defined sink | covered |
| AC-5 No second encoder or shim | no new encode path; only `cfg` gates | by absence in the diff | covered |

## Verdict

Every criterion is backed by a test or a gate that would fail if the
criterion broke. Removing `#![no_std]` support, for example by adding a `std::`
path outside a `cfg`, fails `build-no-std`. Changing an encoding fails the
vector tests in every lane.

Residual, not a defect: byte identity is executed only on the x86_64 host.
The no_std target is build-checked, not run. The float formatter (`ryu-js`) and
the integer paths are pure, target-independent code, so this is argued, not
measured. QSL and RT own any embedded execution lane.

The repo has no spec at all (pre-existing). The new `std` feature therefore
has no owning requirement in-repo. Its rule lives in ADR-013 §2 in QSL. This is
not a defect of this PR.

## Findings

| ID | Severity | Summary | Refs |
| --- | --- | --- | --- |
| FND-001 | low | No findings (placeholder) | - |
