.PHONY: setup install-hooks configure-merge-drivers build build-release check fmt fmt-harn fmt-harn-fix lint lint-md lint-actions lint-harn spec-lint test test-e2e test-cargo test-fast conformance protocol-conformance bench-vm bench-vm-clone bench-llm bench-orchestration all release-gate portal portal-check portal-demo gen-highlight check-highlight gen-trigger-quickref check-trigger-quickref gen-provider-matrix check-provider-matrix gen-connector-matrix check-connector-matrix check-trigger-examples check-docs-snippets sync-language-spec check-language-spec lint-test-patterns check-receipt-structs lint-no-rust-prompt-prose lint-no-xfail-regression

# Full quality check: format first, then lint/test in parallel.
# Usage: make all -j       (parallel checks after formatting)
#        make all           (sequential, also works)
all: fmt
	$(MAKE) lint lint-md lint-actions lint-harn spec-lint fmt-harn test conformance protocol-conformance check-highlight check-language-spec check-trigger-quickref check-provider-matrix check-connector-matrix check-trigger-examples check-docs-snippets lint-test-patterns check-receipt-structs portal-check

check: all

setup:
	./scripts/dev_setup.sh

install-hooks:
	git config core.hooksPath .githooks
	./scripts/configure_merge_drivers.sh

configure-merge-drivers:
	./scripts/configure_merge_drivers.sh

# Build the harn binary. On macOS, ad-hoc signs it so Gatekeeper skips
# the "Verifying harn..." dialog on first run.
build:
	cargo build
	@if [ "$$(uname -s)" = "Darwin" ]; then codesign -s - -f target/debug/harn 2>/dev/null || true; fi

build-release:
	cargo build --release
	@if [ "$$(uname -s)" = "Darwin" ]; then codesign -s - -f target/release/harn 2>/dev/null || true; fi

# Format all code
fmt:
	cargo fmt --all

# Run clippy lints (deny warnings in CI)
lint: lint-no-rust-prompt-prose lint-no-xfail-regression
	cargo clippy --workspace --all-targets -- -D warnings

# Run the fast (in-process, deterministic) test suite via cargo-nextest.
# Subprocess-spawning integration tests are excluded by the nextest "default"
# profile's default-filter. Run `make test-e2e` for the slow E2E suite.
# Falls back to `cargo test --workspace` when nextest is not installed
# (cargo test has no profile support, so it will run all tests).
test:
	@if command -v cargo-nextest >/dev/null 2>&1; then \
		HARN_LLM_CALLS_DISABLED=1 cargo nextest run --workspace; \
	else \
		echo "cargo-nextest not installed; falling back to cargo test --workspace"; \
		echo "hint: run 'make setup' or 'cargo install cargo-nextest --locked'"; \
		HARN_LLM_CALLS_DISABLED=1 cargo test --workspace; \
	fi

# Run the slow E2E / smoke suite: subprocess-spawning CLI surface tests,
# signal handling, MCP server launch, real ProcessHandle smoke tests, etc.
# Runs on schedule (nightly), on the `e2e` PR label, and on merge-queue.
# Requires cargo-nextest (no plain `cargo test` fallback for profile support).
test-e2e:
	HARN_LLM_CALLS_DISABLED=1 cargo nextest run --workspace --profile e2e

# Run the baseline Cargo workspace test command explicitly.
test-cargo:
	HARN_LLM_CALLS_DISABLED=1 cargo test --workspace

# Compatibility alias for the smarter default `make test`.
test-fast:
	@$(MAKE) test

# Run Harn conformance test suite
conformance:
	HARN_LLM_CALLS_DISABLED=1 cargo run --bin harn -- test conformance

protocol-conformance:
	HARN_LLM_CALLS_DISABLED=1 cargo run --bin harn -- test protocols

bench-vm:
	./scripts/bench_vm.sh

bench-vm-clone:
	cargo bench -p harn-vm-perf --bench bench_vmenv_clone -- --output-format bencher

bench-llm:
	cargo bench -p harn-llm-perf --bench bench_llm_options_roundtrip -- --output-format bencher

bench-orchestration:
	cargo bench -p harn-orchestration-perf --bench bench_hook_dispatch -- --output-format bencher

# Lint markdown files
lint-md:
	npx markdownlint-cli2 "**/*.md"

# Validate the Harn Agents Protocol OpenAPI artifact and its public path/schema snapshot.
spec-lint:
	npx redocly lint spec/openapi.yaml
	./scripts/check_openapi_snapshot.py

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
	@harn_bin=$$(cargo metadata --format-version=1 --no-deps | python3 -c 'import json,sys; meta=json.load(sys.stdin); suffix=".exe" if sys.platform == "win32" else ""; print(meta["target_directory"] + "/debug/harn" + suffix)'); \
	workers=$$(getconf _NPROCESSORS_ONLN 2>/dev/null || echo 8); \
	tmp=$$(mktemp -d); \
	status=0; \
	find conformance/tests -name '*.harn' -print0 | \
		TMP_RESULTS="$$tmp" xargs -0 -P "$$workers" -I{} sh -c '\
			error_file="$${1%.harn}.error"; \
			[ -f "$$error_file" ] && exit 0; \
			output=$$("$$0" check "$$1" 2>&1); \
			if echo "$$output" | grep -qE "^.+: (warning|error)\["; then \
				printf "%s\n" "$$output" | grep -v ": ok$$" > "$$TMP_RESULTS/$$(basename "$$1").out"; \
				exit 1; \
			fi' "$$harn_bin" {} || status=$$?; \
	if ls "$$tmp"/*.out >/dev/null 2>&1; then \
		cat "$$tmp"/*.out; \
	fi; \
	rm -rf "$$tmp"; \
	if [ "$$status" -ne 0 ]; then echo "Lint issues found in conformance tests"; exit 1; fi
	@echo "=== Checking Harn experiment support modules ==="
	@cargo run --quiet --bin harn -- check $(EXPERIMENT_HARN_CHECK)
	@echo "    Harn lint OK."

# Check harn formatting on canonical stdlib sources and repo test fixtures.
# Skip syntax cases the formatter intentionally normalizes.
FMT_HARN_SKIP := semicolon_statements.harn semicolon_if_else_invalid.harn semicolon_try_catch_invalid.harn semicolon_empty_statement_invalid.harn
EXPERIMENT_HARN_CHECK := experiments/burin-mini/host.harn experiments/burin-mini/lib/common.harn experiments/burin-mini/lib/profiles.harn
STDLIB_HARN_DIR := crates/harn-stdlib/src/stdlib
fmt-harn-fix:
	@echo "=== Formatting Harn files ==="
	@find $(STDLIB_HARN_DIR) -name '*.harn' -print0 \
		| xargs -0 cargo run --quiet --bin harn -- fmt
	@find conformance/tests -name '*.harn' $(foreach s,$(FMT_HARN_SKIP),-not -name $(s)) -print0 \
		| xargs -0 cargo run --quiet --bin harn -- fmt
	@find experiments -name '*.harn' -print0 \
		| xargs -0 cargo run --quiet --bin harn -- fmt
	@echo "    Harn formatting OK."

fmt-harn:
	@echo "=== Checking Harn formatting ==="
	@find $(STDLIB_HARN_DIR) -name '*.harn' -print0 \
		| xargs -0 cargo run --quiet --bin harn -- fmt --check
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

# Regenerate docs/src/language-spec.md from spec/HARN_SPEC.md (the
# canonical authoring source). Mirrors what release_gate.sh audit's
# sync_language_spec.sh step does.
sync-language-spec:
	./scripts/sync_language_spec.sh

# CI guard: fail if docs/src/language-spec.md is stale relative to
# spec/HARN_SPEC.md. `make sync-language-spec` fixes it.
check-language-spec:
	@echo "=== Checking docs/src/language-spec.md is up to date ==="
	@./scripts/sync_language_spec.sh --check
	@echo "    Language spec mirror OK."

# Regenerate the LLM trigger quickref from the live ProviderCatalog metadata.
gen-trigger-quickref:
	cargo run --quiet -p harn-cli -- dump-trigger-quickref

# CI guard: fail if the trigger quickref is stale relative to ProviderCatalog.
check-trigger-quickref:
	@echo "=== Checking docs/llm/harn-triggers-quickref.md is up to date ==="
	@cargo run --quiet -p harn-cli -- dump-trigger-quickref --check
	@echo "    Harn trigger quickref OK."

# Regenerate the provider/model capability matrix from capabilities.toml.
gen-provider-matrix:
	cargo run --quiet -p harn-cli -- check --provider-matrix --format markdown > docs/src/provider-matrix.md

# CI guard: fail if the provider matrix docs drift from capabilities.toml.
check-provider-matrix:
	@echo "=== Checking docs/src/provider-matrix.md is up to date ==="
	@set -e; \
	tmp=$$(mktemp); \
	trap 'rm -f "$$tmp"' EXIT; \
	cargo run --quiet -p harn-cli -- check --provider-matrix --format markdown > "$$tmp"; \
	if ! python3 scripts/compare_generated_text.py docs/src/provider-matrix.md "$$tmp"; then \
		echo "error: docs/src/provider-matrix.md is stale relative to capabilities.toml" >&2; \
		echo "hint: run 'make gen-provider-matrix' and commit the result" >&2; \
		diff -u docs/src/provider-matrix.md "$$tmp" >&2 || true; \
		exit 1; \
	fi
	@echo "    Harn provider matrix OK."

# Regenerate the connector capability parity matrix from package manifests.
gen-connector-matrix:
	cargo run --quiet -p harn-cli -- dump-connector-matrix

# CI guard: fail if the connector parity docs drift from package manifests.
check-connector-matrix:
	@echo "=== Checking docs/src/connectors/parity-matrix.md is up to date ==="
	@cargo run --quiet -p harn-cli -- dump-connector-matrix --check
	@echo "    Harn connector matrix OK."

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

# Lint test files for wall-clock polling patterns that cause flaky tests.
# See docs/dev/testing.md for approved alternatives and the opt-out mechanism.
lint-test-patterns:
	@./scripts/lint_test_patterns.sh

check-receipt-structs:
	@./scripts/check_receipt_struct_duplication.py

lint-no-rust-prompt-prose:
	@./scripts/check_no_rust_prompt_prose.sh

lint-no-xfail-regression:
	@./scripts/check_xfail_count.sh
