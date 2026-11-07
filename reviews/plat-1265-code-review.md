---
id: SR-2474
title: "Code review of quire-canonical PR 17 writer reservation hook"
type: SpecReview
analysis: code-review
scope: "agent-ix/quire-canonical; PR 17 head be6a28de12db117da3610a2a82edcdf80ee9a085 diff against origin/main: src/reservation.rs, src/writer.rs, src/lib.rs, src/error.rs, docs/encoding.md, tests/reservation.rs, tests/limits.rs"
review_set: subset
---

## Summary

Ticket: PLAT-1265. PR: quire-canonical#17. This is a code review with the
rust-review lane folded in. It covers the additive `Reservation` hook,
`Writer::with_reservation`, `encode_with_reservation`, the shared
`Provision::grow` helper, the release of buffers on refusal, the two-pass
write-out, and the change to when the outermost `{` is written.

Evidence. The focused tests pass: reservation, limits, memory, encode, depth,
value, vectors and drop_value. `cargo clippy --all-targets -D warnings` is
clean. A scratch probe crate (outside the repo, depending on the worktree by
path) ran three shapes the PR's test does not cover:

- A top-level array of N objects, unlimited budget. Charged against peak
  heap: N=1 gives 200/200, N=10 gives 1064/200, N=100 gives 9704/200.
- A top-level serde struct variant (`#[derive(Serialize, FixedShape)] enum E
  { V { b, a } }` through `Writer::serialize`). Every budget from 0 to the
  exact total minus 1 (200 of 200) was refused with output already in the
  sink. At limit 0 the sink holds `{`.
- A duplicate member name of 5 bytes. The writer charges 5 bytes for the
  name, but `collect()` allocates at least 8.

Answers to the brief's questions:

- (a) The second walk runs only when a reservation is given. It repeats the
  same task code with `emit` false, so it writes nothing and leaves the output
  unchanged. On the second walk the task stack never grows. The extra work is
  O(members + child objects) task traversal, with no bytes copied. The cost is
  not stated in the docs (FND-006).
- (b) No consumer under /home/peter/dev depends on the early `{`. I checked
  quire-semantic-value, quire-contract-ir, quire-spec-language,
  quire-observation, quire-driver and quoin. The only custom sinks are
  quire-contract-model `TokenSink` (common.rs:390) and qsl-semantics
  `Spelled` (intake.rs:723). Both encode scalars only and compare byte order,
  not timing. No downstream test asserts `b"{"` left in a sink after an
  error.
- (c) Top-level arrays that stream output before a refusal do not meet AC2
  as written (FND-003). An undocumented second case is worse: a top-level
  serde enum wrapper (FND-002).
- (d) See SR-2475.

## Verdict

**FAIL**: two high findings.

What is sound:

- The hook is called before every `try_reserve_exact`, and the bytes charged
  equal the growth in capacity.
- `release()` on refusal drops every list.
- `AfterRefusal` holds after a denial.
- The two walks share one code path, so there is no second encoder.
- The output is byte-identical with or without a reservation.
- `{`, `,` and `}` are counted before they are written.

The faults are in the accounting contract across more than one outermost
object (FND-001) and in the partial-output promise for the enum wrapper
(FND-002).

## Findings

| ID | Severity | Summary | Refs |
| --- | --- | --- | --- |
| FND-001 | high | After each outermost object, `close_object` drops `open_members`/`open_children` (`mem::take`) and `write_out` drops its local `tasks`. Each top-level array element object then regrows them and is charged again. Charges grow about 96 bytes per element while peak heap stays at 200 bytes (N=100: charged 9704, peak 200). The documented contract ("never asks for the same bytes twice", "sum asked is the peak working heap") is false, and a budget sized to the peak refuses a valid array of objects | src/writer.rs:669-670, src/writer.rs:716-717, src/reservation.rs:24-31, docs/encoding.md:47-51 |
| FND-002 | high | `begin_wrapper` writes `{"Variant":` straight to the sink when the wrapper is the top-level value. A top-level serde enum variant then puts output in the sink on every reservation refusal (probe: 200 of 200 budgets, `{` at limit 0). This contradicts the doc ("a refusal leaves none of it in the sink") and AC2. The case is undocumented and untested | src/writer.rs:851-853, docs/encoding.md:56-58, src/writer.rs:268-274 |
| FND-003 | medium | Elements of a top-level array (scalars, nested arrays, and earlier element objects) reach the sink before a later refusal. This is documented, but AC2 says "a refusal leaves no partial output". With a reservation, buffering a top-level container would meet AC2. Otherwise the owner must accept the deviation. QSL ModelStepOrder (a single top-level object) is not affected | docs/encoding.md:58-59, src/writer.rs:266-267, src/writer.rs:555-559 |
| FND-004 | low | The duplicate-name charge is `Unescaped::count()`, but `collect::<Vec<u8>>()` from an iterator with no size_hint allocates at least 8 bytes and grows by doubling. For a 5-byte name the charge is 5 and the allocation 8. The test passes only because every name in it is exactly 8 bytes (`memberNN`) | src/writer.rs:653-655, tests/reservation.rs:185-186 |
| FND-005 | low | docs/encoding.md says "Without a reservation the writer behaves as before". That is false. For every writer, the outermost `{` now reaches the sink only when the object closes, a refused writer drops its buffers, and growth is now exact doubling from a minimum of 4. tests/limits.rs had to change two assertions | docs/encoding.md:60, tests/limits.rs:79-82, tests/limits.rs:135-138 |
| FND-006 | low | The writer.rs module docs say the outermost object is walked twice with a reservation, but give no cost: one extra O(members + child objects) task traversal and lookup per outermost object. docs/encoding.md does not mention the second walk at all | src/writer.rs:73-76, docs/encoding.md:38-60 |
| FND-007 | low | The charge is the growth in capacity, but a realloc that cannot grow in place holds old plus new capacity during the copy. The test allocator counts realloc as subtract-then-add, so it cannot see that transient peak. "The bytes asked are the bytes allocated" should state this convention | src/reservation.rs:122-127, tests/reservation.rs:46-55 |

## New findings (disposition pass 1)

| ID | Severity | Summary | Refs |
| --- | --- | --- | --- |
| FND-008 | low | docs/encoding.md says that without a reservation "a top-level object is written when it closes". In fact the unmetered writer streams the outermost `{` at `begin_object`: a probe stopped mid-object shows the sink holding `{`, and tests/limits.rs asserts `b"{"` after a refusal. The sentence repeats the reverted round-0 behaviour | docs/encoding.md:69-70, src/writer.rs:400-403, tests/limits.rs:82 |

## Dispositions

Round 1, fix commit 418e06d2a4f749d309d18c9a2ba9a705bc3461ea. The design changed from two walks to held output: a metered writer stages every byte in a charged `staged` list and writes it to the sink in `finish`. Verified with focused tests (reservation, limits, memory, encode, value, depth, drop_value, plus the `--no-default-features` lane), clippy in both feature sets, and the probe crate re-run on this head.

| FND | outcome | reason |
| --- | --- | --- |
| FND-001 | fixed | `open_members`/`open_children` are cleared and given back with their capacity, and `tasks` is now a writer field. Probe: arrays of 1, 10, 100 and 1000 objects make 11, 14, 17 and 20 hook calls (only the output grows), and charged == requested at every size |
| FND-002 | fixed | `begin_wrapper`'s `produce` goes through `emit`, which stages the bytes when a reservation is given. Probe: top-level struct variant, 0 of 272 refused budgets leave output in the sink; the test's "enum wrapper" case asserts the same |
| FND-003 | fixed | Every top-level shape is staged until `finish`. The test's "array", "scalar" and array-of-objects cases assert an empty sink for every smaller limit |
| FND-004 | fixed | The name buffer grows through `Provision::grow` to exactly max(count, 4) and is then extended. The test asserts charged == requested for a 2-byte name, with denial at `requested: 4` |
| FND-005 | fixed | The false sentence is gone. The doc now lists the changes that apply to every writer (exact doubling from a minimum of 4, buffers dropped on refusal), and early-`{` streaming is restored with tests/limits.rs back to main. A residual inaccuracy is FND-008 |
| FND-006 | fixed | The second walk is removed. The docs state the held-output cost: one copy of the output in heap, charged, written once |
| FND-007 | fixed | The unit is now the full new capacity per growth with no credit back, which covers the transient old+new block. The docs and test oracle (charged == bytes requested from the allocator) match |
