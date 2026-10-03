# =============================================================================
# quire-canonical Makefile
# =============================================================================

CARGO ?= cargo
# A target with no `std` at all: building for it proves the crate, built
# without its `std` feature, pulls in nothing from `std`.
NO_STD_TARGET ?= thumbv7em-none-eabi

.PHONY: help
help:
	@echo "Available targets:"
	@echo "  make fmt              - Format with rustfmt"
	@echo "  make fmt-check        - Verify formatting (CI gate)"
	@echo "  make lint             - Clippy with -D warnings"
	@echo "  make test             - cargo test: default (+serde_json), preserve_order and no-std lanes"
	@echo "  make build-no-std     - Build without std for $(NO_STD_TARGET), with and without serde_json"
	@echo "  make build            - Release build"
	@echo "  make clean            - cargo clean"
	@echo "  make deny             - cargo deny check (advisories, bans, licenses, sources)"
	@echo "  make audit-unsafe     - Enforce // SAFETY: comments on unsafe blocks"
	@echo "  make docs             - cargo doc, denying missing/broken doc links"
	@echo "  make ci               - All CI gates locally (fmt-check + lint + test + build-no-std + deny + audit-unsafe + docs)"

# =============================================================================
# Format / Lint / Test
# =============================================================================

.PHONY: fmt
fmt:
	$(CARGO) fmt --all

.PHONY: fmt-check
fmt-check:
	$(CARGO) fmt --all -- --check

.PHONY: lint
lint:
	$(CARGO) clippy --all-targets -- -D warnings
	$(CARGO) clippy --all-targets --all-features -- -D warnings
	$(CARGO) clippy --all-targets --no-default-features -- -D warnings
	$(CARGO) clippy --lib --no-default-features --target $(NO_STD_TARGET) -- -D warnings

# The golden vectors run twice: against serde_json's default BTreeMap-backed
# Map and against its preserve_order (insertion-ordered) Map. The canonical
# bytes must be identical both ways (PLAT-987 AC-3).
.PHONY: test
test: test-default test-preserve-order test-no-std

# With the `serde_json` feature, so the `serde_json::Value` tests run against
# the default `BTreeMap`-backed Map here and the insertion-ordered one in
# test-preserve-order.
.PHONY: test-default
test-default:
	$(CARGO) test --features serde_json

.PHONY: test-preserve-order
test-preserve-order:
	$(CARGO) test --features test-preserve-order

# The suite against the crate built without its `std` feature (no
# `WriteSink`, no `Error::Sink`): encoding into a Vec and SHA-256 minting.
.PHONY: test-no-std
test-no-std:
	$(CARGO) test --no-default-features

.PHONY: build-no-std
build-no-std:
	$(CARGO) build --no-default-features --target $(NO_STD_TARGET)
	$(CARGO) build --no-default-features --features serde_json --target $(NO_STD_TARGET)

.PHONY: build
build:
	$(CARGO) build --release

.PHONY: clean
clean:
	$(CARGO) clean

# =============================================================================
# Supply chain & safety
# =============================================================================

.PHONY: deny
deny:
	$(CARGO) deny check

.PHONY: cargo-audit
cargo-audit:
	$(CARGO) audit

.PHONY: audit-unsafe
audit-unsafe:
	bash scripts/check_unsafe_comments.sh

# =============================================================================
# Documentation
# =============================================================================

# cargo doc is a separate lint pass from clippy: rustdoc-only lints such as
# rustdoc::broken_intra_doc_links only fire here, never under `make lint`.
.PHONY: docs
docs:
	RUSTDOCFLAGS="-D warnings" $(CARGO) doc --no-deps --all-features

# =============================================================================
# Composite
# =============================================================================

.PHONY: ci
ci: fmt-check lint test build-no-std deny audit-unsafe docs
