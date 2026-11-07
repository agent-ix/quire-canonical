---
id: SR-2475
title: "Gap analysis of quire-canonical PR 17 against PLAT-1265 acceptance criteria"
type: SpecReview
analysis: gap-analysis
scope: "agent-ix/quire-canonical; PR 17 head be6a28de12db117da3610a2a82edcdf80ee9a085 diff against origin/main; ticket PLAT-1265 AC1-AC3; consumer context agent-ix/quire-spec-language PR 2489 qsl-analyze/src/monitor_root/order.rs; agent-ix/quire-contract-ir crates/quire-contract-model/src/checked_package/shared.rs"
review_set: subset
---

## Summary

Ticket: PLAT-1265. PR: quire-canonical#17.

quire-canonical has no spec artifacts. `spec/` is empty, and a team-leader
ruling recorded in SR-1241 says the crate has no spec of its own. The
normative text lives in docs/encoding.md and the crate docs, so a planless
`quoin matrix` has no criteria to trace. This analysis is therefore a
manual check of the ticket's acceptance criteria against the tests.
Plan completion: not assessed. Tools available: quoin 0.29.4, quire 0.38.3.

AC to test trace:

- **AC1** (reservation hook called before, and able to refuse, each internal
  allocation): covered for the event writer, through `Provision::grow` at
  every list growth. Tested by
  `reservation_charges_exactly_the_writers_heap_and_a_refusal_leaves_nothing`
  (total charged == measured peak). Not covered for the crate's own `Encode`
  impls (FND-001).
- **AC2** (no partial output or retained buffers on refusal; exact budget
  admits, one byte under refuses): covered only for one top-level object.
  Every limit from 0 to total-1 is denied, the sink is empty and live heap is
  unchanged. Top-level arrays, top-level enum wrappers and arrays of objects
  are not tested (FND-003; the defects are SR-2474 FND-001/002/003).
- **AC3** (value-asserting tests, a no-allocation assertion, spec lines in
  the same PR, no second encoder, no fake after-charge): met. The spec lines
  are in docs/encoding.md and the writer.rs module docs, which is the repo's
  actual normative location given the no-spec ruling, so `spec/` is not
  needed. The second walk reuses the same code. Every charge comes before its
  allocation.

Oracle (brief question d). Peak heap is the right oracle. The charge is the
growth in capacity and realloc frees the old block, so a "total allocated"
oracle would count every realloc's full new size and could never equal the
charge. The oracle does go red if an internal growth bypasses the hook:
nothing is freed before the walk's task stack peaks, so an uncharged growth
raises the peak above the total. That holds only for growths the one test
tree exercises.

Consumer (brief question f). QSL #2489 `ModelStepOrder` encodes `OrderValue`
through the event API: one top-level object with nested objects. So it does
not hit `serde_json::Value`, `Document`, `NodeRef`, arrays or enum wrappers.
It does call `CheckedNodeId::encode_into` (quire-contract-model
shared.rs:1167-1205). That impl allocates its own `pending` stack
(`vec!`/`extend`), and QSL itself allocates in `structural_id(key)` and in
`period.numerator().to_string()`. None of these is visible to the writer's
hook (FND-002).

## Verdict

**FAIL** for AC2 as written (see SR-2474). AC1 is met only partly, for the
crate's own `Encode` impls. AC3 is met.

## Findings

| ID | Severity | Summary | Refs |
| --- | --- | --- | --- |
| FND-001 | medium | AC1 asks for the hook before each internal allocation. The crate's own `Encode` impls for `serde_json::Value` (stack, `try_reserve`) and `Document`/`NodeRef` (task stack) grow their own lists without the reservation. `encode_with_reservation(&value, …)` on those types under-charges, silently and without limit | src/value.rs:62-97, src/read.rs:388-440, src/lib.rs:218-237 |
| FND-002 | medium | `Writer` has no public method that lets an `Encode` impl charge its own working heap through the caller's reservation. The `CheckedNodeId` `pending` stack, which ModelStepOrder reaches, cannot be precharged through the writer. IR-1383 must charge it out of band or leave it uncharged | src/writer.rs:268-274, src/reservation.rs:188-197 |
| FND-003 | medium | The value tests cover a single top-level object only. No reservation test covers a top-level array, an array of objects, a top-level serde enum, or a short duplicate name, which are exactly the shapes behind SR-2474 FND-001/002/003/004 | tests/reservation.rs:154-237 |
