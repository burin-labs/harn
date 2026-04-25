.PHONY: setup install-hooks configure-merge-drivers check fmt fmt-harn fmt-harn-fix lint lint-md lint-actions lint-harn test test-cargo test-fast conformance bench-vm all release-gate portal portal-check portal-demo gen-highlight check-highlight gen-trigger-quickref check-trigger-quickref check-trigger-examples check-docs-snippets

# Full quality check: format first, then lint/test in parallel.
# Usage: make all -j       (parallel checks after formatting)
#        make all           (sequential, also works)
all: fmt
	$(MAKE) lint lint-md lint-actions lint-harn fmt-harn test conformance check-highlight check-trigger-quickref check-trigger-examples check-docs-snippets portal-check

check: all

setup:
	./scripts/dev_setup.sh

install-hooks:
	git config core.hooksPath .githooks
	./scripts/configure_merge_drivers.sh

configure-merge-drivers:
	./scripts/configure_merge_drivers.sh

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
		HARN_LLM_CALLS_DISABLED=1 cargo nextest run --workspace; \
	else \
		echo "cargo-nextest not installed; falling back to cargo test --workspace"; \
		echo "hint: run 'make setup' or 'cargo install cargo-nextest --locked'"; \
		HARN_LLM_CALLS_DISABLED=1 cargo test --workspace; \
	fi

# Run the baseline Cargo workspace test command explicitly.
test-cargo:
	HARN_LLM_CALLS_DISABLED=1 cargo test --workspace

# Compatibility alias for the smarter default `make test`.
test-fast:
	@$(MAKE) test

# Run Harn conformance test suite
conformance:
	HARN_LLM_CALLS_DISABLED=1 cargo run --bin harn -- test conformance

bench-vm:
	./scripts/bench_vm.sh

# Lint markdown files
lint-md:
	npx markdownlint-cli2 "**/*.md"

# Lint GitHub Actions workflows.
lint-actions:
	@if command -v actionlint >/dev/null 2>&1; then \
		actionlint; \
	else \
		echo "actionlint not installed; skipping GitHub Actions lint"; \
		echo "hint: brew install actionlint or go install github.com/rhysd/actionlint/cmd/actionlint@v1.7.12"; \
	fi

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
	@echo "=== Checking Harn experiment support modules ==="
	@cargo run --quiet --bin harn -- check $(EXPERIMENT_HARN_CHECK)
	@echo "    Harn lint OK."

# Check harn formatting on conformance tests.
# Skip syntax cases the formatter intentionally normalizes.
FMT_HARN_SKIP := semicolon_statements.harn semicolon_if_else_invalid.harn semicolon_try_catch_invalid.harn semicolon_empty_statement_invalid.harn
EXPERIMENT_HARN_CHECK := experiments/burin-mini/host.harn experiments/burin-mini/lib/common.harn experiments/burin-mini/lib/profiles.harn
fmt-harn-fix:
	@echo "=== Formatting Harn files ==="
	@find conformance/tests -name '*.harn' $(foreach s,$(FMT_HARN_SKIP),-not -name $(s)) -print0 \
		| xargs -0 cargo run --quiet --bin harn -- fmt
	@find experiments -name '*.harn' -print0 \
		| xargs -0 cargo run --quiet --bin harn -- fmt
	@echo "    Harn formatting OK."

fmt-harn:
	@echo "=== Checking Harn formatting ==="
	@find conformance/tests -name '*.harn' $(foreach s,$(FMT_HARN_SKIP),-not -name $(s)) -print0 \
		| xargs -0 cargo run --quiet --bin harn -- fmt --check
	@find experiments -name '*.harn' -print0 \
		| xargs -0 cargo run --quiet --bin harn -- fmt --check
	@echo "    Harn formatting OK."

# Format check (no changes, for CI)
fmt-check:
	cargo fmt --all -- --check

release-gate:
	./scripts/release_gate.sh audit

# Build-verify the portal frontend (TypeScript type check + Vite bundle).
# Requires npm dependencies: run `make setup` or `cd crates/harn-cli/portal && npm install`.
portal-check:
	@echo "=== Checking portal frontend build ==="
	cd crates/harn-cli/portal && npm run lint && npm run build
	@echo "    Portal build OK."

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

# Regenerate the LLM trigger quickref from the live ProviderCatalog metadata.
gen-trigger-quickref:
	cargo run --quiet -p harn-cli -- dump-trigger-quickref

# CI guard: fail if the trigger quickref is stale relative to ProviderCatalog.
check-trigger-quickref:
	@echo "=== Checking docs/llm/harn-triggers-quickref.md is up to date ==="
	@cargo run --quiet -p harn-cli -- dump-trigger-quickref --check
	@echo "    Harn trigger quickref OK."

# Validate the ready-to-customize trigger example library.
check-trigger-examples:
	@echo "=== Checking trigger examples ==="
	@find examples/triggers -mindepth 1 -maxdepth 1 -type d | sort | while IFS= read -r dir; do \
		test -f "$$dir/harn.toml"; \
		test -f "$$dir/lib.harn"; \
		test -f "$$dir/README.md"; \
		test -f "$$dir/SKILL.md"; \
		cargo run --quiet --bin harn -- check "$$dir/lib.harn"; \
	done
	@echo "    Trigger examples OK."

# CI guard: every ```harn block in docs/src/*.md must parse under
# `harn check`. Blocks tagged ```harn,ignore are skipped.
check-docs-snippets:
	@echo "=== Checking docs snippets parse under harn check ==="
	@./scripts/check_docs_snippets.sh
