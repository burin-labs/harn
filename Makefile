.PHONY: setup install-hooks check fmt fmt-harn lint lint-md lint-harn test conformance all release-gate portal

# Full quality check: format first, then lint/test in parallel.
# Usage: make all -j       (parallel checks after formatting)
#        make all           (sequential, also works)
all: fmt
	$(MAKE) lint lint-md lint-harn fmt-harn test conformance

check: all

setup:
	./scripts/dev_setup.sh

install-hooks:
	git config core.hooksPath .githooks

# Format all code
fmt:
	cargo fmt --all

# Run clippy lints (deny warnings in CI)
lint:
	cargo clippy --workspace -- -D warnings

# Run Rust unit tests
test:
	cargo test --workspace

# Run Harn conformance test suite
conformance:
	cargo run --bin harn -- test conformance

# Lint markdown files
lint-md:
	npx markdownlint-cli2 "**/*.md"

# Lint Harn conformance tests (check for warnings)
lint-harn:
	@echo "=== Linting Harn conformance tests ==="
	@cargo build --quiet --bin harn
	@workers=$$(getconf _NPROCESSORS_ONLN 2>/dev/null || echo 8); \
	tmp=$$(mktemp -d); \
	status=0; \
	find conformance/tests -name '*.harn' -print0 | \
		TMP_RESULTS="$$tmp" xargs -0 -P "$$workers" -I{} sh -c '\
			output=$$(target/debug/harn check "$$1" 2>&1); \
			if echo "$$output" | grep -qE "^.+: (warning|error)\["; then \
				printf "%s\n" "$$output" | grep -v ": ok$$" > "$$TMP_RESULTS/$$(basename "$$1").out"; \
				exit 1; \
			fi' sh {} || status=$$?; \
	if ls "$$tmp"/*.out >/dev/null 2>&1; then \
		cat "$$tmp"/*.out; \
	fi; \
	rm -rf "$$tmp"; \
	if [ "$$status" -ne 0 ]; then echo "Lint issues found in conformance tests"; exit 1; fi
	@echo "    Harn lint OK."

# Check harn formatting on conformance tests (CI, not pre-commit)
fmt-harn:
	@echo "=== Checking Harn formatting ==="
	@cargo run --quiet --bin harn -- fmt --check conformance/tests/
	@echo "    Harn formatting OK."

# Format check (no changes, for CI)
fmt-check:
	cargo fmt --all -- --check

release-gate:
	./scripts/release_gate.sh audit

portal:
	cargo run --bin harn -- portal
