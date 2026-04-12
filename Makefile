.PHONY: setup install-hooks check fmt fmt-harn lint lint-md lint-harn test test-cargo test-fast conformance all release-gate portal portal-check portal-demo gen-highlight check-highlight check-docs-snippets

# Full quality check: format first, then lint/test in parallel.
# Usage: make all -j       (parallel checks after formatting)
#        make all           (sequential, also works)
all: fmt
	$(MAKE) lint lint-md lint-harn fmt-harn test conformance check-highlight check-docs-snippets portal-check

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
	cargo clippy --workspace --all-targets -- -D warnings

# Run Rust unit tests via cargo-nextest when available for better whole-workspace
# parallelism and bounded timeouts (see .config/nextest.toml). Falls back to
# `cargo test --workspace` when nextest is not installed.
test:
	@if command -v cargo-nextest >/dev/null 2>&1; then \
		cargo nextest run --workspace; \
	else \
		echo "cargo-nextest not installed; falling back to cargo test --workspace"; \
		echo "hint: run 'make setup' or 'cargo install cargo-nextest --locked'"; \
		cargo test --workspace; \
	fi

# Run the baseline Cargo workspace test command explicitly.
test-cargo:
	cargo test --workspace

# Compatibility alias for the smarter default `make test`.
test-fast:
	@$(MAKE) test

# Run Harn conformance test suite
conformance:
	cargo run --bin harn -- test conformance

# Lint markdown files
lint-md:
	npx markdownlint-cli2 "**/*.md"

# Lint Harn conformance tests (check for warnings).
# Skip .harn files that have a paired .error file — those are intentional
# error tests whose diagnostics are validated by the conformance runner.
lint-harn:
	@echo "=== Linting Harn conformance tests ==="
	@cargo build --quiet --bin harn
	@workers=$$(getconf _NPROCESSORS_ONLN 2>/dev/null || echo 8); \
	tmp=$$(mktemp -d); \
	status=0; \
	find conformance/tests -name '*.harn' -print0 | while IFS= read -r -d '' f; do \
		error_file="$${f%.harn}.error"; \
		[ -f "$$error_file" ] && continue; \
		printf '%s\0' "$$f"; \
	done | \
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

# Check harn formatting on conformance tests (CI, not pre-commit).
# Skip tests that exercise syntax the formatter intentionally normalizes
# (triple-quoted multiline strings → single-line escaped strings).
FMT_HARN_SKIP := multiline_strings.harn multiline_string_interpolation.harn
fmt-harn:
	@echo "=== Checking Harn formatting ==="
	@find conformance/tests -name '*.harn' $(foreach s,$(FMT_HARN_SKIP),-not -name $(s)) -print0 \
		| xargs -0 cargo run --quiet --bin harn -- fmt --check
	@echo "    Harn formatting OK."

# Format check (no changes, for CI)
fmt-check:
	cargo fmt --all -- --check

release-gate:
	./scripts/release_gate.sh audit

# Build-verify the portal frontend (TypeScript type check + Vite bundle).
# Requires npm dependencies to be installed (make setup handles this).
portal-check:
	@if [ -d crates/harn-cli/portal/node_modules ]; then \
		echo "=== Checking portal frontend build ==="; \
		cd crates/harn-cli/portal && npm run lint && npm run build; \
		echo "    Portal build OK."; \
	else \
		echo "=== Skipping portal check (node_modules not installed) ==="; \
	fi

portal:
	cargo run --bin harn -- portal

portal-demo:
	./scripts/portal_demo.sh

# Regenerate docs/theme/harn-keywords.js from the live lexer + stdlib.
# Run this whenever keywords or globally-available builtins change.
gen-highlight:
	cargo run --quiet -p harn-cli -- dump-highlight-keywords

# CI guard: fail if docs/theme/harn-keywords.js is stale relative to
# the lexer/stdlib. `make gen-highlight` fixes it.
check-highlight:
	@echo "=== Checking docs/theme/harn-keywords.js is up to date ==="
	@cargo run --quiet -p harn-cli -- dump-highlight-keywords --check
	@echo "    Harn keyword file OK."

# CI guard: every ```harn block in docs/src/*.md must parse under
# `harn check`. Blocks tagged ```harn,ignore are skipped.
check-docs-snippets:
	@echo "=== Checking docs snippets parse under harn check ==="
	@./scripts/check_docs_snippets.sh
