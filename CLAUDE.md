# quire-canonical

Streaming RFC 8785 (JCS) canonical JSON writer that hashes as it encodes.

## Hash / digest / pin antipattern: present, slated for removal

This repository still contains hashes, digests, SHAs, pins, checksum catalogs and
records that track files, versions or tools. That is an antipattern and it is slated
for removal. Do not introduce any new use of it. When you touch code or specs that
contain one, remove it as part of the change. The only hash that stays is a canonical
identity digest that binds a proof to the exact content it proved. Package versions
live in Cargo.toml / package.json and their lockfiles only; reports name the app
version they ran.

## Commands

```bash
make fmt            # format with rustfmt
make fmt-check      # verify formatting (CI gate)
make lint           # clippy with -D warnings
make test           # cargo test: default, serde_json/preserve_order and --no-default-features lanes
make build-no-std   # build without std for thumbv7em-none-eabi (proves no_std + alloc)
make build          # release build
make clean          # cargo clean
make deny           # cargo deny check (advisories, bans, licenses, sources)
make audit-unsafe   # check that every unsafe block has a // SAFETY: comment
make ci             # fmt-check + lint + test + build-no-std + deny + audit-unsafe + docs
```

## Safety scaffolding

- `deny.toml` allow-lists licenses and denies unknown registries/git sources
- `scripts/check_unsafe_comments.sh` runs in CI and locally via `make audit-unsafe`. Every `unsafe {` block must have a `// SAFETY:` comment within the 3 preceding lines, or be listed in `scripts/unsafe_comment_baseline.txt`. Update the baseline with `bash scripts/check_unsafe_comments.sh --update-baseline`.
- `rustfmt.toml` uses 100-char width (stable options only). CI fails on drift.

## Layout

```
src/lib.rs             # crate root
quire-canonical-derive/ # #[derive(FixedShape)] proc-macro crate (workspace member)
tests/vectors.rs       # golden vectors (tests/vectors/jcs-vectors.json)
tests/limits.rs        # byte limits and the heap stacks they bound
tests/depth.rs         # 100,000-deep values on a 512 KiB stack
tests/read.rs          # the shared JSON reader and its refusals
tests/encode.rs        # serde data model mapping, event API and refusals
tests/value.rs         # serde_json::Value (`serde_json` feature; preserve_order lane)
tests/derive.rs        # #[derive(FixedShape)]; a recursive type fails with E0391
tests/memory.rs        # peak heap per shape, writer and reader
benches/               # criterion benchmarks (opt-in; add criterion to dev-deps)
spec/                  # requirements artifacts (from /spec-create-spec)
scripts/               # local tooling
```
