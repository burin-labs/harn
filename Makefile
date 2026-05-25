.PHONY: setup install-hooks configure-merge-drivers build build-release sign-local check fmt fmt-harn fmt-harn-fix lint lint-md lint-actions lint-harn spec-lint test test-e2e test-cargo test-fast test-harn-scripts conformance protocol-conformance mcp-rc-conformance replay-oracle replay-bench eval-tool-calls bench-vm bench-vm-micro bench-vm-clone bench-llm bench-orchestration bench-cli-cold-start all release-gate release-smoke smoke-audit portal portal-check portal-demo gen-highlight check-highlight gen-protocol-artifacts check-protocol-artifacts check-burin-protocol-artifacts check-bindings gen-session-bundle-schema check-session-bundle-schema gen-trigger-quickref check-trigger-quickref gen-provider-matrix check-provider-matrix gen-provider-support check-provider-support gen-provider-catalog check-provider-catalog gen-connector-matrix check-connector-matrix check-trigger-examples check-docs-snippets check-docs-workflow-quickstart sync-language-spec check-language-spec sync-diagnostics-catalog check-diagnostics-catalog lint-test-patterns lint-diagnostic-codes check-receipt-structs lint-no-rust-prompt-prose lint-no-xfail-regression check-provider-catalog-drift check-ported-handler-loc

# Full quality check: format first, then lint/test in parallel.
# Usage: make all -j       (parallel checks after formatting)
#        make all           (sequential, also works)
all: fmt
	$(MAKE) lint lint-md lint-actions lint-harn spec-lint fmt-harn test test-harn-scripts conformance protocol-conformance mcp-rc-conformance replay-oracle replay-bench check-highlight check-protocol-artifacts check-bindings check-session-bundle-schema check-language-spec check-trigger-quickref check-provider-matrix check-provider-support check-provider-catalog check-connector-matrix check-trigger-examples check-docs-snippets check-docs-workflow-quickstart check-diagnostics-catalog lint-test-patterns lint-diagnostic-codes check-receipt-structs check-provider-catalog-drift portal-check

check: all

setup:
	./scripts/dev_setup.sh

install-hooks:
	git config core.hooksPath .githooks
	./scripts/configure_merge_drivers.sh

configure-merge-drivers:
	./scripts/configure_merge_drivers.sh

# Build the harn binary. On macOS, signs it (Developer ID Application if
# the team cert is in the login keychain, ad-hoc otherwise) so Gatekeeper
# skips the "Verifying harn..." dialog on first run. Single source of
# truth: scripts/sign_local_macos.sh.
build:
	cargo build
	@HARN_LOCAL_SIGN_QUIET=1 ./scripts/sign_local_macos.sh

build-release:
	cargo build --release
	@HARN_LOCAL_SIGN_QUIET=1 ./scripts/sign_local_macos.sh

# Re-sign already-built harn binaries without rebuilding. Useful after
# pulling, switching worktrees, or any path that touched target/ without
# going through `make build` (e.g. `cargo run` with sccache).
sign-local:
	./scripts/sign_local_macos.sh

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
# Runs on schedule (nightly), manually, and on PRs with the `e2e` label.
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

# MCP RC compatibility harness: exercises Harn's MCP client against fake
# RC servers, fake RC clients against the generic and orchestrator
# servers, and validates the published wire fixtures + JSON Schema
# 2020-12 recursive `$defs` handling.
#
# Failures are scoped per surface so CI breakage attribution is
# unambiguous:
#   - tests/client.rs           — fake-server self-consistency
#   - tests/generic_server.rs   — generic harn-serve MCP server
#   - tests/legacy_compat.rs    — 2025-11-25 wire compat regression
#   - tests/artifacts.rs        — published fixtures + recursive $defs
#   - harn-cli mcp_rc_compat_tests — orchestrator MCP server
mcp-rc-conformance:
	@echo "=== MCP RC harness: harn-mcp-rc-compat suite (client / generic_server / legacy_compat / artifacts) ==="
	HARN_LLM_CALLS_DISABLED=1 cargo test -p harn-mcp-rc-compat --tests
	@echo "=== MCP RC harness: orchestrator server (harn-cli mcp_rc_compat_tests) ==="
	HARN_LLM_CALLS_DISABLED=1 cargo test -p harn-cli --lib mcp_rc_compat_tests

replay-oracle:
	HARN_LLM_CALLS_DISABLED=1 cargo run --bin harn -- orchestrator replay-oracle

replay-bench:
	HARN_LLM_CALLS_DISABLED=1 cargo run --bin harn -- bench replay --json >/dev/null

eval-tool-calls:
	cargo run --bin harn -- eval tool-calls --dataset conformance/tool-call-eval --planner mock:mock --output .harn-runs/tool-call-eval/latest

bench-vm:
	./scripts/bench_vm.sh

bench-vm-micro:
	./scripts/bench_vm_micro.sh

bench-vm-clone:
	cargo bench -p harn-vm-perf --bench bench_vmenv_clone -- --output-format bencher

bench-llm:
	cargo bench -p harn-llm-perf --bench bench_llm_options_roundtrip -- --output-format bencher

bench-orchestration:
	cargo bench -p harn-orchestration-perf --bench bench_hook_dispatch -- --output-format bencher

bench-cli-cold-start:
	./scripts/bench_cli_cold_start.sh

# Lint markdown files
lint-md:
	npx markdownlint-cli2 "**/*.md"

# Validate the Harn Agents Protocol OpenAPI artifact and its public path/schema snapshot.
spec-lint:
	npx redocly lint spec/openapi.yaml
	cargo run --quiet --bin harn -- run scripts/check_openapi_snapshot.harn

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
	@echo "=== Linting Harn-authored scripts ==="
	@cargo run --quiet --bin harn -- lint scripts/*.harn scripts/tests/*.harn
	@echo "=== Linting bundled demo scenarios ==="
	@cargo run --quiet --bin harn -- lint crates/harn-cli/assets/demo
	@echo "=== Checking stdlib metadata contract (HARN-STD-101) ==="
	@harn_bin=$$(cargo metadata --format-version=1 --no-deps | python3 -c 'import json,sys; meta=json.load(sys.stdin); suffix=".exe" if sys.platform == "win32" else ""; print(meta["target_directory"] + "/debug/harn" + suffix)'); \
	tmp=$$(mktemp); \
	find crates/harn-stdlib/src/stdlib -name '*.harn' -print0 | xargs -0 "$$harn_bin" lint > "$$tmp" 2>&1 || true; \
	if grep -q 'HARN-STD-101' "$$tmp"; then \
		grep -E 'HARN-STD-101|^crates/' "$$tmp" | head -40; \
		rm -f "$$tmp"; \
		echo "HARN-STD-101 warnings found above — fix or backfill (see scripts/backfill_stdlib_metadata.py)"; \
		exit 1; \
	fi; \
	rm -f "$$tmp"
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
	@find scripts -name '*.harn' -print0 \
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
	@find scripts -name '*.harn' -print0 \
		| xargs -0 cargo run --quiet --bin harn -- fmt --check
	@find crates/harn-cli/assets/demo -name '*.harn' -print0 \
		| xargs -0 cargo run --quiet --bin harn -- fmt --check
	@echo "    Harn formatting OK."

# Run the @test pipelines that cover scripts/*.harn against pure-logic
# fixtures (no filesystem dependency outside the canonical spec mirror
# check). Wired into `make all` and exercised by CI.
test-harn-scripts:
	@echo "=== Running Harn script test suite ==="
	@cargo run --quiet --bin harn -- test scripts/tests/
	@echo "    Harn script tests OK."

# Format check (no changes, for CI)
fmt-check:
	cargo fmt --all -- --check

release-gate:
	./scripts/release_gate.sh audit

# Local reproduction of the release-smoke CI matrix. Builds a release
# `harn` binary for the host platform and runs the cross-platform smoke
# driver against it. CI runs this on macOS, Linux, and Windows; locally
# you only get the host platform, but the driver still exercises every
# user-visible capability and prints a per-step status summary.
release-smoke:
	CARGO_PROFILE_RELEASE_LTO=thin cargo build --release -p harn-cli --bin harn
	./scripts/release_smoke.sh

# Faster `release-smoke` variant that reuses the debug `harn` binary.
# Used by the parallel audit lanes in release_gate.sh because the warm
# prebuild already populated target/debug; rebuilding release would
# fight the cargo lock with rust-audit's clippy + nextest.
smoke-audit:
	HARN_BINARY=$(or $(CARGO_TARGET_DIR),target)/debug/harn$(if $(filter Windows_NT,$(OS)),.exe,) ./scripts/release_smoke.sh

# Build-verify the portal frontend (TypeScript type check + Vite bundle).
# The repo-root npm scripts bootstrap portal dependencies when needed so this
# target works in fresh worktrees.
portal-check:
	@echo "=== Checking portal frontend build ==="
	npm run portal:lint
	npm run portal:test
	npm run portal:build
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

gen-protocol-artifacts:
	cargo run --quiet -p harn-cli -- dump-protocol-artifacts

check-protocol-artifacts:
	@echo "=== Checking Harn protocol artifacts are up to date ==="
	@cargo run --quiet -p harn-cli -- dump-protocol-artifacts --check
	@echo "    Harn protocol artifacts OK."

check-burin-protocol-artifacts:
	@echo "=== Checking Burin Code vendored protocol bindings match Harn ==="
	@python3 scripts/check_burin_protocol_bindings.py --required
	@echo "    Burin protocol bindings OK."

# Round-trip the published JSON fixture through the Python and Go protocol
# bindings to catch wire-vocabulary drift before downstream consumers vendor
# the artifacts. Skips the Go half if the toolchain is missing so contributors
# without Go installed locally are not blocked, but CI requires both.
check-bindings:
	@echo "=== Checking Harn protocol bindings round-trip the published fixture ==="
	@python3 -m py_compile spec/protocol-artifacts/python/harn_protocol.py
	@python3 scripts/check_protocol_bindings.py
	@if command -v go >/dev/null 2>&1; then \
		stale=$$(gofmt -l spec/protocol-artifacts/go/harnprotocol); \
		if [ -n "$$stale" ]; then \
			echo "error: gofmt would change:"; echo "$$stale"; exit 1; \
		fi; \
		(cd spec/protocol-artifacts/go/harnprotocol && go vet ./... && go test ./...); \
	else \
		echo "    skipping go round-trip (go not installed)"; \
	fi
	@echo "    Harn protocol bindings OK."

gen-session-bundle-schema:
	cargo run --quiet -p harn-cli -- session schema --out spec/schemas/session-bundle.v1.schema.json

check-session-bundle-schema:
	@echo "=== Checking session bundle schema is up to date ==="
	@cargo run --quiet -p harn-cli -- session schema --check
	@echo "    Session bundle schema OK."

# Regenerate docs/src/language-spec.md from spec/HARN_SPEC.md (the
# canonical authoring source). Mirrors what release_gate.sh audit's
# sync_language_spec.harn step does.
sync-language-spec:
	cargo run --quiet --bin harn -- run scripts/sync_language_spec.harn

# CI guard: fail if docs/src/language-spec.md is stale relative to
# spec/HARN_SPEC.md. `make sync-language-spec` fixes it.
check-language-spec:
	@echo "=== Checking docs/src/language-spec.md is up to date ==="
	@cargo run --quiet --bin harn -- run scripts/sync_language_spec.harn -- --check
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
	cargo run --quiet --bin harn -- providers matrix

# CI guard: fail if the provider matrix docs drift from capabilities.toml.
check-provider-matrix:
	@echo "=== Checking docs/src/provider-matrix.md is up to date ==="
	@cargo run --quiet --bin harn -- providers matrix --check
	@echo "    Harn provider matrix OK."

# Regenerate provider support recommendations from catalog/capabilities/notes.
gen-provider-support:
	cargo run --quiet --bin harn -- providers support

# CI guard: fail if provider support markdown or JSON drift.
check-provider-support:
	@echo "=== Checking provider support artifacts are up to date ==="
	@cargo run --quiet --bin harn -- providers support --check
	@echo "    Harn provider support OK."

# Regenerate the checked-in provider/model catalog JSON, schema, and downstream bindings.
gen-provider-catalog:
	cargo run --quiet --bin harn -- providers export

# CI guard: fail if checked-in provider/model catalog artifacts drift.
check-provider-catalog:
	@echo "=== Checking provider catalog artifacts ==="
	@cargo run --quiet --bin harn -- providers validate --check-artifacts
	@echo "    Harn provider catalog artifacts OK."

# Regenerate the connector capability parity matrix from package manifests.
gen-connector-matrix:
	cargo run --quiet -p harn-cli -- dump-connector-matrix

# CI guard: fail if the connector parity docs drift from package manifests.
check-connector-matrix:
	@echo "=== Checking docs/src/connectors/parity-matrix.md is up to date ==="
	@cargo run --quiet -p harn-cli -- dump-connector-matrix --check
	@echo "    Harn connector matrix OK."

# CI guard: replay the provider catalog refresh workflow against bundled
# HTTP fixtures and verify the rendered drift report + candidate TOML
# match the committed goldens. After intentional adapter or fixture
# changes, run
#   cargo run --quiet --bin harn -- run scripts/update_provider_catalog.harn -- --check --update
# and commit the regenerated files under scripts/provider_catalog_fixtures/.
check-provider-catalog-drift:
	@echo "=== Checking provider catalog refresh workflow ==="
	@cargo run --quiet --bin harn -- run scripts/update_provider_catalog.harn -- --check
	@echo "    Provider catalog refresh OK."

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

# Regenerate the diagnostic-code catalog (markdown page + JSON sidecar)
# from the in-binary registry in crates/harn-parser/src/diagnostic_codes.rs.
# Run this whenever you add, rename, retire, or rewire a HARN-<CAT>-<NNN>
# code or its repair template.
sync-diagnostics-catalog:
	cargo run --quiet --bin harn -- explain --catalog --format markdown > docs/src/diagnostics.md
	cargo run --quiet --bin harn -- explain --catalog --format json > docs/diagnostics-catalog.json

# CI guard: fail if docs/src/diagnostics.md or docs/diagnostics-catalog.json
# drift from the in-binary registry. `make sync-diagnostics-catalog` fixes it.
check-diagnostics-catalog:
	@echo "=== Checking diagnostic-code catalog is up to date ==="
	@set -e; \
	tmp_md=$$(mktemp); \
	tmp_json=$$(mktemp); \
	trap 'rm -f "$$tmp_md" "$$tmp_json"' EXIT; \
	cargo run --quiet --bin harn -- explain --catalog --format markdown > "$$tmp_md"; \
	cargo run --quiet --bin harn -- explain --catalog --format json > "$$tmp_json"; \
	if ! diff -u docs/src/diagnostics.md "$$tmp_md" >/dev/null; then \
		echo "error: docs/src/diagnostics.md is stale relative to the diagnostic-code registry." >&2; \
		echo "hint: run 'make sync-diagnostics-catalog' and commit the result." >&2; \
		diff -u docs/src/diagnostics.md "$$tmp_md" >&2 || true; \
		exit 1; \
	fi; \
	if ! diff -u docs/diagnostics-catalog.json "$$tmp_json" >/dev/null; then \
		echo "error: docs/diagnostics-catalog.json is stale relative to the diagnostic-code registry." >&2; \
		echo "hint: run 'make sync-diagnostics-catalog' and commit the result." >&2; \
		diff -u docs/diagnostics-catalog.json "$$tmp_json" >&2 || true; \
		exit 1; \
	fi
	@echo "    Diagnostic-code catalog OK."

# CI guard: every ```harn block in docs/src/*.md must parse under
# `harn check`. Blocks tagged ```harn,ignore are skipped.
check-docs-snippets:
	@echo "=== Checking docs snippets parse under harn check ==="
	@./scripts/check_docs_snippets.sh

# CI guard: the workflow-authoring quickstart fixtures still produce the
# bundle digests, executed-node sequences, and connector-status shapes
# the docs claim.
check-docs-workflow-quickstart:
	@./scripts/check_docs_workflow_quickstart.sh

# Lint test files for wall-clock polling patterns that cause flaky tests.
# See docs/src/dev/testing.md for approved alternatives and the opt-out mechanism.
lint-test-patterns:
	@./scripts/lint_test_patterns.sh

lint-diagnostic-codes:
	@python3 scripts/check_diagnostic_codes.py

check-receipt-structs:
	@./scripts/check_receipt_struct_duplication.py

lint-no-rust-prompt-prose:
	@./scripts/check_no_rust_prompt_prose.sh

lint-no-xfail-regression:
	@cargo run --quiet --bin harn -- run scripts/check_xfail_count.harn

# CI ratchet: fail if any Rust CLI handler listed in
# scripts/ported_handlers.toml grows past its budgeted LOC. Tracks
# epic #2293 (subticket #2314 = C1). See the script header for how to
# add new entries / adjust budgets.
check-ported-handler-loc:
	@cargo run --quiet --bin harn -- run scripts/check_ported_handler_loc.harn
