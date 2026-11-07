---
id: SR-3406
title: "Code review of quire-canonical PR 18 reader working-byte provision"
type: SpecReview
analysis: code-review
scope: "agent-ix/quire-canonical; PR 18 head 57fcb89e47fcaa1aa427219a2cf8d1e889741984 diff against origin/code/plat1265-writer-preflight (eec0a028): src/read.rs, src/reservation.rs, src/error.rs, src/lib.rs, docs/encoding.md, tests/read_reservation.rs"
review_set: subset
---

## Summary

Ticket: PLAT-1247. PR: quire-canonical#18. Code review with the rust-review
lane folded in. Only PR 18's own diff (three-dot against its base branch,
PR 17) was reviewed.

The PR adds `read_with_reservation` and `read_bounded`, charges every reader
list growth through the writer's `Provision` (`grow`, new `grow_text`, new
shared `admit`), replaces the up-front `source.len()` text reservation with
lazy charged growth, adds `ReadError::ReservationDenied`,
`LimitKind::WorkingBytes` and `Document::retained_bytes`, and documents the
unit and retention in docs/encoding.md and the read.rs module docs.

Evidence. Release-mode focused tests pass: read_reservation, memory, read,
reservation, depth. `cargo clippy --all-targets -D warnings` and
`cargo fmt --check` are clean. Three local mutation probes, each reverted:

- `escape()` appending decoded text with an uncharged `push_str`
  (src/read.rs:1100): read_reservation and read stay green.
- `pointer()` pushing `steps` with an uncharged `reserve(1)`
  (src/read.rs:720,725): read_reservation and read stay green.
- `close()` growing `members` with an uncharged `reserve_exact`
  (src/read.rs:981-983): read_reservation goes red at line 167.

Answers to the brief's questions:

- Out-of-budget NumberOutOfRange returning ReservationDenied: acceptable.
  The pointer and lexeme are real growth the caller asked to meter; refusing
  that growth as configured denial is the AC2 contract. In budget the typed
  refusal is identical to the unmetered reader (tested). No finding.
- "live heap at reserve <= charged": it does not catch an uncharged
  allocation after the last reserve call, nor one that fits in the slack of
  charged-but-freed realloc blocks (FND-003). Only the retained-heap check on
  one shape covers the end of the read, and only for retained memory.
- `retained_bytes` is exactly the heap the Document holds (capacity times
  element size of its four lists; tested equal to live heap after the read).
  It is the right figure for a consumer to keep charged. Heap held by a
  NumberOutOfRange error is not covered by the retention text (FND-007).
- Lazy text: the unmetered reader's measured peaks stay under the 64x bound
  (max 45.88x, deep arrays 10000). Text capacity is at most twice the decoded
  length, which never exceeds the source, so behaviour is unchanged and the
  bound holds. No finding.
- Uncharged paths: none found by inspection in the reader. Every Vec/String
  growth goes through `Provision`; `Malformed` refusals allocate nothing;
  `NodeRef::encode_into` charges its task stack through the writer.

## Verdict

**PASS WITH FINDINGS**: no high findings. The implementation is correct by
inspection and by the focused tests. Two medium test gaps leave two charged
paths (escaped text, the out-of-range pointer) unprotected against regression.

## Findings

| ID | Severity | Summary | Refs |
| --- | --- | --- | --- |
| FND-001 | medium | Charging of decoded escape text is untested. No SHAPES input contains an escape, so replacing `self.append(decoded...)` with an uncharged `self.text.push_str(...)` leaves read_reservation and read green (mutation probe). AC2 names decoded-text growth; a regression there ships unseen | src/read.rs:1100, tests/read_reservation.rs:112-119 |
| FND-002 | medium | Charging of the NumberOutOfRange pointer `steps` list is untested. Replacing both `provision.grow(&mut steps, 1)` with `steps.reserve(1)` stays green: the only test denies the last request and asserts only the variant, never the request sequence, the denied bytes or live heap 0 after the refusal | src/read.rs:720, src/read.rs:725, tests/read_reservation.rs:258-268 |
| FND-003 | low | The "live at reserve <= charged" oracle is necessary but not sufficient: charges are never credited, so after a realloc frees its old block an uncharged allocation up to that slack passes, and an allocation after the last reserve is never checked. `PEAK` is recorded by the test allocator but never asserted; asserting peak-during-read <= total charged would check the documented bound directly | tests/read_reservation.rs:22, tests/read_reservation.rs:163-172 |
| FND-004 | low | `from_provision` maps any other `Error` variant to `ReadError::Allocation { requested: 0 }` with a catch-all arm. Provision returns only the two mapped variants today; a new variant would silently become a false zero-byte allocation failure. An explicit unreachable-by-construction note or exhaustive mapping is clearer | src/read.rs:127-133 |
| FND-005 | low | `LimitKind` is a public enum without `#[non_exhaustive]`, and the PR adds `WorkingBytes` with the crate still at 0.3.0. A downstream exhaustive `match` on it stops compiling. Mark it non_exhaustive or bump the minor version with the release | src/error.rs:15-23, Cargo.toml:3 |
| FND-006 | low | Dead code in the test: `let (_, ample, _) = recorded(b"[[1e400]]", None); let _ = ample;` does nothing | tests/read_reservation.rs:259-260 |
| FND-007 | low | The retention text covers the Document and the dropped scratch lists but not a `NumberOutOfRange` error, which keeps the charged pointer and lexeme Strings after the read returns. A refunding meter following the docs would release that charge while the error still holds it | docs/encoding.md:96-102, src/read.rs:35-44 |
