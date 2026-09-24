# =============================================================================
# quire-canonical Makefile
# =============================================================================

CARGO ?= cargo

.PHONY: help
help:
	@echo "Available targets:"
	@echo "  make fmt              - Format with rustfmt"
	@echo "  make fmt-check        - Verify formatting (CI gate)"
	@echo "  make lint             - Clippy with -D warnings"
	@echo "  make test             - cargo test, default and preserve_order lanes"
	@echo "  make build            - Release build"
	@echo "  make clean            - cargo clean"
	@echo "  make deny             - cargo deny check (advisories, bans, licenses, sources)"
	@echo "  make audit-unsafe     - Enforce // SAFETY: comments on unsafe blocks"
	@echo "  make docs             - cargo doc, denying missing/broken doc links"
	@echo "  make ci               - All CI gates locally (fmt-check + lint + test + deny + audit-unsafe + docs)"

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

# The golden vectors run twice: against serde_json's default BTreeMap-backed
# Map and against its preserve_order (insertion-ordered) Map. The canonical
# bytes must be identical both ways (PLAT-987 AC-3).
.PHONY: test
test: test-default test-preserve-order

.PHONY: test-default
test-default:
	$(CARGO) test

.PHONY: test-preserve-order
test-preserve-order:
	$(CARGO) test --features test-preserve-order

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
ci: fmt-check lint test deny audit-unsafe docs
