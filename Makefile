.PHONY: check fmt fmt-harn lint lint-md lint-harn test conformance all

# Full quality check: format, lint, test, conformance
all: fmt fmt-harn lint lint-md lint-harn test conformance

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
	@fail=0; for f in conformance/tests/*.harn; do \
		output=$$(cargo run --quiet --bin harn -- check "$$f" 2>&1); \
		if echo "$$output" | grep -qE '^.+: (warning|error)\['; then \
			echo "$$output" | grep -v ": ok$$"; \
			fail=1; \
		fi; \
	done; \
	if [ "$$fail" = "1" ]; then echo "Lint issues found in conformance tests"; exit 1; fi
	@echo "    Harn lint OK."

# Check harn formatting on conformance tests (CI, not pre-commit)
fmt-harn:
	@echo "=== Checking Harn formatting ==="
	@fail=0; for f in conformance/tests/*.harn; do \
		output=$$(cargo run --quiet --bin harn -- fmt --check "$$f" 2>&1); \
		if echo "$$output" | grep -q "would be reformatted"; then \
			echo "  $$output"; \
			fail=1; \
		fi; \
	done; \
	if [ "$$fail" = "1" ]; then echo "Harn format issues found"; exit 1; fi
	@echo "    Harn formatting OK."

# Format check (no changes, for CI)
fmt-check:
	cargo fmt --all -- --check
