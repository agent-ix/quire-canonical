---
id: SR-1241
title: "QSL-480 gap analysis of quire-canonical PR #6 against QSL FR-259 and ADR-030 D-4.5"
type: SpecReview
analysis: gap-analysis
scope: "agent-ix/quire-canonical@0955507f6a99f8782227a89741f86e089e700964; PR #6 diff against origin/main (9572a21); contract on agent-ix/quire-spec-language@10c32f5132d59164e276cf663203b1bdf0575fe7: spec/functional/FR-259-encode-identities-and-read-json-through-quire-canonical-at-any-depth.md (Capabilities QSL relies on), spec/decisions/ADR-030-arbitrary-nesting-depth-no-fixed-caps.md (D-4.5)"
review_set: subset
---
# QSL-480 gap analysis of quire-canonical PR #6

## Summary

Ticket: QSL-480. PR: quire-canonical#6. quire-canonical has no spec of its
own (team-leader ruling), so the contract is the four capabilities that QSL
FR-259 and ADR-030 D-4.5 name. The ticket also has its own acceptance.

Capability to test trace:

- **Byte-only encoding, byte error distinct from malformed input.** Covered:
  - `deep_opens_are_refused_on_bytes_not_depth`,
    `nested_object_buffers_are_refused_on_bytes` and
    `deep_document_encoding_is_refused_on_bytes` all give
    `LimitKind::CanonicalBytes`.
  - `reader_input_byte_limit_is_checked_first_and_is_not_malformed_input`
    gives `ReadError::Limit(InputBytes)`, never `Malformed`.
- **Event API for input-depth data, driven from the caller's own stack.**
  Covered:
  - `event_api_encodes_a_100000_deep_value_on_a_small_stack` runs on a
    512 KiB stack, and its bytes equal the 8 MiB run and an expected text
    built independently.
  - `event_api_refuses_out_of_order_events` and
    `event_api_finish_requires_one_complete_value` cover the protocol.
- **Reader of untrusted JSON.** Covered:
  - Non-recursive traits: `deep_document_traits_do_not_recurse`.
  - Malformed input refused with its byte offset: tests/read.rs, including
    the lone surrogate and `1e400` that FR-259-AC-3 names.
  - Input byte limit: tests/limits.rs.
  - Encoder for the tree: `reader_and_tree_digest_a_100000_deep_document_on_a_small_stack`,
    against SHA-256 over canonical text built independently.
- **A serde path only fixed-depth types can take.** The honest recursive case
  is covered by the `compile_fail` doctest. The literal-`DEPTH` case is not
  covered and does compile (FND-001).
- **Ticket acceptance, "Each new heap stack's growth is covered by an
  existing node or byte charge (test)".** Covered for the writer stack, the
  frames and the reader. The tree encoder's task stack is bounded by the
  document already in memory.
- **Ticket acceptance, "No compatibility layer, shim, re-export or
  fallback".** Met. No alias for any deleted name is left, and
  `Limits::new` takes only `max_bytes`.

## Verdict

Changes requested: one medium and one low coverage gap.

## Findings

| ID | Severity | Summary | Refs |
| --- | --- | --- | --- |
| FND-001 | medium | FR-259 capability 4 ("a serde encoding path that only fixed-depth types can take") is backed only by a test where `DEPTH` refers to the type itself. A `FixedShape` with a literal `DEPTH` over a recursive value compiles and takes the serde path; reproduced, it overflows the stack. Nothing records this limit of the guarantee. Same root cause as SR-1240 FND-001. Fix there: docs and a derive. | src/shape.rs:15-43; src/encoder.rs:31-35 |
| FND-002 | low | tests/memory.rs, the only peak-heap test, covers a flat object and a two-level object of objects. It does not cover a deep object chain, the shape with the largest heap per canonical byte (about 32-39x). So the documented memory bound has no test for the case it most needs. Related to SR-1240 FND-002. Fix: add a deep-object case with the bound the docs will state. | tests/memory.rs:64-99 |
