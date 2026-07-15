.PHONY: setup clean-stale-targets install-hooks configure-merge-drivers build build-release sign-local check fmt fmt-harn fmt-harn-fix lint lint-md lint-actions lint-harn spec-lint test test-e2e test-cargo test-fast test-harn-scripts test-agent-scripts test-pr-gate-scripts conformance mechanism-contracts protocol-conformance mcp-rc-conformance replay-oracle replay-bench eval-tool-calls bench-vm bench-vm-micro bench-vm-clone check-vm-rss-soak bench-llm bench-orchestration bench-cli-cold-start loadgen-postgres all release-gate release-smoke smoke-audit portal portal-check portal-demo gen-cli-aot check-cli-aot gen-highlight check-highlight gen-protocol-artifacts check-protocol-artifacts gen-connector-schemas check-connector-schemas check-burin-protocol-artifacts check-bindings gen-session-bundle-schema check-session-bundle-schema gen-run-view-fixtures check-run-view-fixtures gen-trigger-quickref check-trigger-quickref gen-provider-matrix check-provider-matrix check-provider-support check-provider-catalog check-connector-matrix check-trigger-examples check-docs-model-refs check-docs-snippets check-docs-cli-flags check-docs-links check-site-snippets check-docs-workflow-quickstart sync-language-spec check-language-spec sync-diagnostics-catalog check-diagnostics-catalog lint-test-patterns lint-diagnostic-codes check-stdlib-strict-types check-stdlib-public-return-types check-receipt-structs lint-no-rust-prompt-prose lint-agent-path-normalization lint-no-xfail-regression check-provider-catalog-drift check-ported-handler-loc check-source-file-lengths update-source-file-length-baseline check-python-boundary check-harn-syntax-sensitive-scans check-crate-sibling-versions check-dependabot-groups gen-tree-sitter-keywords check-tree-sitter-keywords check-grammar-keywords check-generated-registry check-release-audit-contract check-ci-cache-policy

HARN_BIN ?=
HARN_CARGO_CMD = ./scripts/cargo_with_worktree_build_dir.sh
HARN_BIN_CMD = ./scripts/harn_bin.sh
HARN_BIN_PRINT_CMD = $(if $(strip $(HARN_BIN)),env HARN_BIN="$(HARN_BIN)" $(HARN_BIN_CMD) --print,$(HARN_BIN_CMD) --print)
HARN_CMD = $(if $(strip $(HARN_BIN)),env HARN_BIN="$(HARN_BIN)" $(HARN_BIN_CMD) --,$(HARN_BIN_CMD) --)
HARN_CMD_VERBOSE = $(HARN_CMD)
HARN_CLI_CMD = $(HARN_CMD)
HARN_BIN_ASSIGN = harn_bin="$$($(HARN_BIN_PRINT_CMD))"

# Full quality check: format first, then lint/test in parallel.
# Usage: make all -j       (parallel checks after formatting)
#        make all           (sequential, also works)
all: fmt
	$(MAKE) lint lint-md lint-actions lint-harn spec-lint fmt-harn test test-harn-scripts test-agent-scripts test-pr-gate-scripts conformance protocol-conformance mcp-rc-conformance replay-oracle replay-bench check-cli-aot check-highlight check-protocol-artifacts check-connector-schemas check-bindings check-session-bundle-schema check-run-view-fixtures check-language-spec check-trigger-quickref check-provider-matrix check-provider-support check-provider-catalog check-connector-matrix check-trigger-examples check-docs-model-refs check-docs-snippets check-docs-cli-flags check-docs-links check-site-snippets check-docs-workflow-quickstart check-diagnostics-catalog lint-test-patterns lint-diagnostic-codes check-stdlib-strict-types check-stdlib-public-return-types check-receipt-structs check-provider-catalog-drift check-source-file-lengths check-python-boundary check-harn-syntax-sensitive-scans check-crate-sibling-versions check-dependabot-groups check-tree-sitter-keywords check-grammar-keywords check-generated-registry check-release-audit-contract check-ci-cache-policy portal-check

check: all

setup:
	./scripts/dev_setup.sh

# Reclaim orphaned per-worktree Cargo target dirs under $TMPDIR/harn-target
# (left behind when an agent/codex worktree is removed). Add --dry-run to
# preview: make clean-stale-targets ARGS=--dry-run
clean-stale-targets:
	./scripts/prune_stale_targets.sh $(ARGS)

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
	$(HARN_CARGO_CMD) build
	@HARN_LOCAL_SIGN_QUIET=1 ./scripts/sign_local_macos.sh

build-release:
	$(HARN_CARGO_CMD) build --release
	@HARN_LOCAL_SIGN_QUIET=1 ./scripts/sign_local_macos.sh

# Re-sign already-built harn binaries without rebuilding. Useful after
# pulling, switching worktrees, or any path that touched target/ without
# going through `make build` (e.g. `cargo run` with sccache).
sign-local:
	./scripts/sign_local_macos.sh

# Format all code
fmt:
	$(HARN_CARGO_CMD) fmt --all

# Run clippy lints (deny warnings in CI)
lint: lint-no-rust-prompt-prose lint-no-xfail-regression
	$(HARN_CARGO_CMD) clippy --workspace --all-targets -- -D warnings

# Detect unused workspace dependencies. cargo-machete is fast and good enough
# for CI; false positives can be silenced via [package.metadata.cargo-machete]
# in the relevant crate's Cargo.toml. Skips silently if not installed locally.
lint-deps:
	@if command -v cargo-machete >/dev/null 2>&1; then \
		cargo machete; \
	else \
		echo "cargo-machete not installed; \`cargo install --locked cargo-machete\` to enable"; \
	fi

# Ruff lint for the Python scripts that ship alongside the Rust crates
# (scripts/, conformance/helpers/, tests/). Config is in pyproject.toml.
# Skips silently if Ruff is not installed locally — CI installs it explicitly.
lint-py:
	@if command -v ruff >/dev/null 2>&1; then \
		ruff check scripts/ conformance/helpers/ tests/; \
	else \
		echo "ruff not installed; \`pip install ruff\` or \`brew install ruff\` to enable"; \
	fi

check-crate-sibling-versions:
	$(HARN_CMD) run scripts/check_crate_sibling_versions.harn

check-dependabot-groups:
	$(HARN_CMD) run scripts/check_dependabot_groups.harn

# Run the fast (in-process, deterministic) test suite via cargo-nextest.
# Subprocess-spawning integration tests are excluded by the nextest "default"
# profile's default-filter. Run `make test-e2e` for the slow E2E suite.
# Falls back to `cargo test --workspace` when nextest is not installed
# (cargo test has no profile support, so it will run all tests).
test:
	@if command -v cargo-nextest >/dev/null 2>&1; then \
		HARN_LLM_CALLS_DISABLED=1 $(HARN_CARGO_CMD) nextest run --workspace; \
	else \
		echo "cargo-nextest not installed; falling back to cargo test --workspace"; \
		echo "hint: run 'make setup' or 'cargo install cargo-nextest --locked'"; \
		HARN_LLM_CALLS_DISABLED=1 $(HARN_CARGO_CMD) test --workspace; \
	fi

# Run only the tests in crates affected by the changes vs AFFECTED_BASE
# (default origin/main), expanded by the reverse-dependency closure. Used on
# `pull_request` CI for fast feedback (#2663). The merge queue runs the FULL
# `make test` instead — see `.github/workflows/ci.yml`.
# A global/workspace-level change (Cargo.lock, .cargo/, toolchain, etc.)
# falls back to the full workspace automatically. Requires cargo-nextest. The
# selector is intentionally buildless so PR CI can choose packages before
# compiling the Harn CLI.
AFFECTED_BASE ?= origin/main
test-affected:
	@command -v cargo-nextest >/dev/null 2>&1 || { \
		echo "test-affected requires cargo-nextest; run 'make setup'"; exit 1; }
	@args="$$(./scripts/ci/affected_crate_args.sh --base "$(AFFECTED_BASE)")"; \
	if [ -z "$$args" ]; then \
		echo "make test-affected: no affected crates; skipping Rust tests."; \
		exit 0; \
	fi; \
	echo "make test-affected: cargo nextest run $$args"; \
	HARN_LLM_CALLS_DISABLED=1 $(HARN_CARGO_CMD) nextest run $$args

# Run the slow E2E / smoke suite: subprocess-spawning CLI surface tests,
# signal handling, MCP server launch, real ProcessHandle smoke tests, etc.
# Runs on schedule (nightly), manually, and on PRs with the `e2e` label.
# Requires cargo-nextest (no plain `cargo test` fallback for profile support).
test-e2e:
	HARN_LLM_CALLS_DISABLED=1 $(HARN_CARGO_CMD) nextest run --workspace --profile e2e --run-ignored all

# Run the baseline Cargo workspace test command explicitly.
test-cargo:
	HARN_LLM_CALLS_DISABLED=1 $(HARN_CARGO_CMD) test --workspace

# Compatibility alias for the smarter default `make test`.
test-fast:
	@$(MAKE) test

# Run Harn conformance test suite
conformance:
	HARN_LLM_CALLS_DISABLED=1 $(HARN_CMD_VERBOSE) test conformance

# Mechanism-contract onramp tier: the manufactured mini-evals that prove a new
# termination/escalation/judge/guard/routing mechanism ENGAGES correctly (fires
# on its trigger, emits its effect, does NOT fire on the negative case) before
# any N>=5 convergence gauntlet. A focused, fast filter over the
# conformance/tests/mechanisms/*.contract.harn suite — already covered by
# `make conformance`, broken out here for authoring and as the documented gate.
# See conformance/tests/mechanisms/README.md.
mechanism-contracts:
	HARN_LLM_CALLS_DISABLED=1 $(HARN_CMD_VERBOSE) test conformance --filter '.contract'

protocol-conformance:
	HARN_LLM_CALLS_DISABLED=1 $(HARN_CMD_VERBOSE) test protocols

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
#
# This target is a developer-convenience entry point for local iteration
# on the MCP surface — it lets you re-run the focused suite without
# building the whole workspace. CI does NOT call it: the two `cargo test`
# invocations below are a strict subset of `cargo nextest run --workspace`
# in the `rust-test` workflow job, so running it again in `harn-audit`
# would pay ~4 min of wall-clock to redo work the workspace test run
# already covers.
mcp-rc-conformance:
	@echo "=== MCP RC harness: harn-mcp-rc-compat suite (client / generic_server / legacy_compat / artifacts) ==="
	HARN_LLM_CALLS_DISABLED=1 $(HARN_CARGO_CMD) test -p harn-mcp-rc-compat --tests
	@echo "=== MCP RC harness: orchestrator server (harn-cli mcp_rc_compat_tests) ==="
	HARN_LLM_CALLS_DISABLED=1 $(HARN_CARGO_CMD) test -p harn-cli --lib mcp_rc_compat_tests

replay-oracle:
	HARN_LLM_CALLS_DISABLED=1 $(HARN_CMD_VERBOSE) orchestrator replay-oracle

replay-bench:
	HARN_LLM_CALLS_DISABLED=1 $(HARN_CMD_VERBOSE) bench replay --json >/dev/null

eval-tool-calls:
	$(HARN_CMD_VERBOSE) eval tool-calls --dataset conformance/tool-call-eval --planner mock:mock --output .harn-runs/tool-call-eval/latest

bench-vm:
	./scripts/bench_vm.sh

bench-vm-micro:
	./scripts/bench_vm_micro.sh

bench-vm-clone:
	cargo bench -p harn-vm-perf --bench bench_vmenv_clone -- --output-format bencher

check-vm-rss-soak:
	$(HARN_CMD) run scripts/check_vm_rss_soak.harn

bench-llm:
	cargo bench -p harn-llm-perf --bench bench_llm_options_roundtrip -- --output-format bencher

bench-orchestration:
	cargo bench -p harn-orchestration-perf --bench bench_hook_dispatch -- --output-format bencher

bench-cli-cold-start:
	./scripts/bench_cli_cold_start.sh

# Postgres hostlib loadgen. Self-skips (exit 0) when HARN_TEST_POSTGRES_URL
# is unset; see perf/postgres/README.md for the tunable env vars.
loadgen-postgres:
	cargo run --release -p harn-postgres-perf --bin harn-postgres-loadgen

# Lint markdown files
lint-md:
	npx markdownlint-cli2 "**/*.md"

# Validate the Harn Agents Protocol OpenAPI artifact and its public path/schema snapshot.
spec-lint:
	npx redocly lint spec/openapi.yaml
	$(HARN_CMD) run scripts/check_openapi_snapshot.harn

# Lint GitHub Actions workflows.
lint-actions:
	@if command -v actionlint >/dev/null 2>&1; then \
		actionlint; \
	else \
		echo "actionlint not installed; skipping GitHub Actions lint"; \
		echo "hint: brew install actionlint or go install github.com/rhysd/actionlint/cmd/actionlint@v1.7.12"; \
	fi
	@if grep -R -n -E 'uses: burin-labs/\.github/\.github/workflows/runner-availability\.yml@' .github/workflows \
		| grep -v -E '@[0-9a-f]{40}([[:space:]]|$$)'; then \
		echo "Pin org runner-availability workflow references to a full commit SHA." >&2; \
		exit 1; \
	fi

# Lint Harn conformance tests (check for warnings).
# Skip .harn files that have a paired .error file — those are intentional
# error tests whose diagnostics are validated by the conformance runner.
lint-harn:
	@echo "=== Linting Harn conformance tests ==="
	@harn_bin="$$($(HARN_BIN_PRINT_CMD))"; \
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
	@$(HARN_CMD) check $(EXPERIMENT_HARN_CHECK)
	@echo "=== Linting Harn-authored scripts ==="
	@$(HARN_CMD) lint --strict scripts/*.harn scripts/tests/*.harn
	@echo "=== Linting bundled demo scenarios ==="
	@$(HARN_CMD) lint --strict crates/harn-cli/assets/demo
	@echo "=== Checking stdlib metadata contract (HARN-STD-101) ==="
	@harn_bin="$$($(HARN_BIN_PRINT_CMD))"; \
	tmp=$$(mktemp); \
	find crates/harn-stdlib/src/stdlib -name '*.harn' -print0 | xargs -0 "$$harn_bin" lint > "$$tmp" 2>&1 || true; \
	if grep -q 'HARN-STD-101' "$$tmp"; then \
		grep -E 'HARN-STD-101|^crates/' "$$tmp" | head -40; \
		rm -f "$$tmp"; \
		echo "HARN-STD-101 warnings found above — fix or backfill (see scripts/backfill_stdlib_metadata.harn)"; \
		exit 1; \
	fi; \
	rm -f "$$tmp"
	@echo "    Harn lint OK."

# Check harn formatting on canonical stdlib sources and repo test fixtures.
# Skip syntax cases the formatter intentionally normalizes.
FMT_HARN_SKIP := semicolon_statements.harn semicolon_if_else_invalid.harn semicolon_try_catch_invalid.harn semicolon_empty_statement_invalid.harn import_broken_module_lib.harn
EXPERIMENT_HARN_CHECK := experiments/burin-mini/host.harn experiments/burin-mini/lib/common.harn experiments/burin-mini/lib/profiles.harn
STDLIB_HARN_DIR := crates/harn-stdlib/src/stdlib
# Extra repo-root directories that contain user-facing .harn fixtures
# but were historically outside the fmt-harn gate. Keeping them in the
# gate avoids the "I edited persona X and pre-commit reformatted three
# unrelated files" surprise from accumulated drift.
EXTRA_HARN_DIRS := personas tests examples evals
EXTRA_HARN_FIND := find $(EXTRA_HARN_DIRS) -type d -name .harn -prune -o -type f -name '*.harn' -print0 2>/dev/null

fmt-harn-fix:
	@echo "=== Formatting Harn files ==="
	@find $(STDLIB_HARN_DIR) -name '*.harn' -print0 \
		| xargs -0 $(HARN_CMD) fmt
	@find conformance/tests -name '*.harn' $(foreach s,$(FMT_HARN_SKIP),-not -name $(s)) -print0 \
		| xargs -0 $(HARN_CMD) fmt
	@find experiments -name '*.harn' -print0 \
		| xargs -0 $(HARN_CMD) fmt
	@find scripts -name '*.harn' -print0 \
		| xargs -0 $(HARN_CMD) fmt
	@$(EXTRA_HARN_FIND) \
		| xargs -0 -r $(HARN_CMD) fmt
	@echo "    Harn formatting OK."

fmt-harn:
	@echo "=== Checking Harn formatting ==="
	@find $(STDLIB_HARN_DIR) -name '*.harn' -print0 \
		| xargs -0 $(HARN_CMD) fmt --check
	@find conformance/tests -name '*.harn' $(foreach s,$(FMT_HARN_SKIP),-not -name $(s)) -print0 \
		| xargs -0 $(HARN_CMD) fmt --check
	@find experiments -name '*.harn' -print0 \
		| xargs -0 $(HARN_CMD) fmt --check
	@find scripts -name '*.harn' -print0 \
		| xargs -0 $(HARN_CMD) fmt --check
	@find crates/harn-cli/assets/demo -name '*.harn' -print0 \
		| xargs -0 $(HARN_CMD) fmt --check
	@$(EXTRA_HARN_FIND) \
		| xargs -0 -r $(HARN_CMD) fmt --check
	@echo "    Harn formatting OK."

# Run the @test pipelines that cover scripts/*.harn against pure-logic
# fixtures (no filesystem dependency outside the canonical spec mirror
# check). Wired into `make all` and exercised by CI.
test-harn-scripts:
	@echo "=== Running Harn script test suite ==="
	@$(HARN_CMD) test scripts/tests/
	@$(HARN_CMD) test experiments/burin-mini/tests/
	@echo "    Harn script tests OK."

# Agent-loop Harn unit tests (stall detector, loop control, judge verdict,
# transcript helpers). These live outside `conformance/` so they are NOT walked
# by `make conformance`; this target wires them into CI so they cannot rot.
test-agent-scripts:
	@echo "=== Running Harn agent-loop test suite ==="
	@HARN_LLM_CALLS_DISABLED=1 $(HARN_CMD) test tests/agent/
	@echo "    Harn agent-loop tests OK."

test-pr-gate-scripts:
	./scripts/tests/ci_docs_only_test.sh
	./scripts/tests/ci_release_metadata_only_test.sh
	./scripts/tests/ci_merge_group_proof_test.sh
	./scripts/tests/changelog_fragment_check_test.sh
	./scripts/tests/release_ship_fragment_guard_test.sh
	./scripts/tests/merge_group_path_gate_test.sh
	./scripts/tests/nextest_filters_from_paths_test.sh
	./scripts/tests/affected_crate_args_test.sh
	./scripts/tests/hook_no_local_build_mode_test.sh
	./scripts/tests/hook_rust_gate_test.sh
	./scripts/tests/pre_push_validation_range_test.sh
	./scripts/tests/ci_rust_test_lane_test.sh
	./scripts/tests/ci_preemption_recover_test.sh
	./scripts/tests/audit_gates_parallel_test.sh
	./scripts/tests/ci_harn_bin_warm_test.sh
	./scripts/tests/harn_bin_resolver_test.sh
	./scripts/tests/harn_launcher_python_cutover_test.sh
	./scripts/tests/claude_dev_setup_once_test.sh
	./scripts/tests/bench_vm_startup_test.sh
	./scripts/tests/make_harn_cargo_env_test.sh
	./scripts/tests/cargo_build_dir_isolation_test.sh
	./scripts/tests/release_gate_harn_bin_test.sh
	./scripts/tests/release_prepare_env_test.sh
	./scripts/tests/publish_script_test.sh
	./scripts/tests/report_ci_cache_budget_test.sh

# Format check (no changes, for CI)
fmt-check:
	$(HARN_CARGO_CMD) fmt --all -- --check

release-gate:
	./scripts/release_gate.sh audit

# Local reproduction of the release-smoke CI matrix. Builds a release
# `harn` binary for the host platform and runs the cross-platform smoke
# driver against it. CI runs this on macOS, Linux, and Windows; locally
# you only get the host platform, but the driver still exercises every
# user-visible capability and prints a per-step status summary.
release-smoke:
	CARGO_PROFILE_RELEASE_LTO=thin $(HARN_CARGO_CMD) build --release -p harn-cli --bin harn
	./scripts/release_smoke.sh

# Faster `release-smoke` variant that reuses the debug `harn` binary.
# Used by the parallel audit lanes in release_gate.sh because the warm
# prebuild already populated Cargo's active target dir; rebuilding release would
# fight the cargo lock with rust-audit's clippy + nextest.
smoke-audit:
	@harn_binary="$(HARN_BIN)" && \
	if [ -z "$$harn_binary" ]; then \
		harn_binary="$$($(HARN_BIN_PRINT_CMD))"; \
	fi && \
	if [ ! -x "$$harn_binary" ]; then \
		if [ -n "$(strip $(HARN_BIN))" ]; then \
			echo "HARN_BIN is not executable: $$harn_binary" >&2; \
			exit 1; \
		fi; \
		CARGO_PROFILE_DEV_DEBUG=0 $(HARN_CARGO_CMD) build -p harn-cli --bin harn; \
	fi && \
	HARN_BINARY="$$harn_binary" ./scripts/release_smoke.sh

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
	$(HARN_CMD_VERBOSE) portal

portal-demo:
	./scripts/portal_demo.sh

# Regenerate committed bytecode for the Harn-backed CLI commands. The generator
# compiles harn-vm in Cargo's normal graph; harn-cli's build script only verifies
# and embeds these outputs.
gen-cli-aot:
	$(HARN_CARGO_CMD) run -p harn-cli-aot-gen

check-cli-aot:
	@echo "=== Checking committed CLI AOT artifacts ==="
	@$(HARN_CARGO_CMD) run -p harn-cli-aot-gen -- --check
	@echo "    CLI AOT artifacts OK."

# Regenerate docs/theme/harn-keywords.js from the live lexer + stdlib.
# Run this whenever keywords or globally-available builtins change.
gen-highlight:
	$(HARN_CLI_CMD) dump-highlight-keywords

# CI guard: fail if docs/theme/harn-keywords.js is stale relative to
# the lexer/stdlib. `make gen-highlight` fixes it.
check-highlight:
	@echo "=== Checking docs/theme/harn-keywords.js is up to date ==="
	@$(HARN_CLI_CMD) dump-highlight-keywords --check
	@echo "    Harn keyword file OK."

gen-protocol-artifacts:
	$(HARN_CLI_CMD) dump-protocol-artifacts

check-protocol-artifacts:
	@echo "=== Checking Harn protocol artifacts are up to date ==="
	@$(HARN_CLI_CMD) dump-protocol-artifacts --check
	@echo "    Harn protocol artifacts OK."

gen-connector-schemas:
	$(HARN_CLI_CMD) connector-schema-codegen

check-connector-schemas:
	@echo "=== Checking generated connector event schemas are up to date ==="
	@$(HARN_CLI_CMD) connector-schema-codegen --check
	@echo "    Connector event schemas OK."

check-burin-protocol-artifacts:
	@echo "=== Checking Burin Code vendored protocol bindings match Harn ==="
	@$(HARN_CLI_CMD) run --no-sandbox scripts/check_burin_protocol_bindings.harn -- --required
	@echo "    Burin protocol bindings OK."

# Round-trip the published JSON fixture through the Python and Go protocol
# bindings to catch wire-vocabulary drift before downstream consumers vendor
# the artifacts. Skips the Go half if the toolchain is missing so contributors
# without Go installed locally are not blocked, but CI requires both.
check-bindings:
	@echo "=== Checking Harn protocol bindings round-trip the published fixture ==="
	@$(HARN_CLI_CMD) run scripts/check_protocol_bindings.harn
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
	$(HARN_CLI_CMD) session schema --out spec/schemas/session-bundle.v1.schema.json

check-session-bundle-schema:
	@echo "=== Checking session bundle schema is up to date ==="
	@$(HARN_CLI_CMD) session schema --check
	@echo "    Session bundle schema OK."

gen-run-view-fixtures:
	HARN_REGENERATE_RUN_VIEW_FIXTURES=1 $(HARN_CARGO_CMD) test -p harn-vm --test run_view_fixtures -- run_view_fixture_snapshots_match --exact

check-run-view-fixtures:
	@echo "=== Checking run/session view fixture snapshots ==="
	@$(HARN_CARGO_CMD) test -p harn-vm --test run_view_fixtures -- run_view_fixture_snapshots_match --exact
	@echo "    Run/session view fixtures OK."

# Regenerate docs/src/language-spec.md from spec/HARN_SPEC.md (the
# canonical authoring source). Mirrors what release_gate.sh audit's
# sync_language_spec.harn step does.
sync-language-spec:
	$(HARN_CMD) run scripts/sync_language_spec.harn

# CI guard: fail if docs/src/language-spec.md is stale relative to
# spec/HARN_SPEC.md. `make sync-language-spec` fixes it.
check-language-spec:
	@echo "=== Checking docs/src/language-spec.md is up to date ==="
	@$(HARN_CMD) run scripts/sync_language_spec.harn -- --check
	@echo "    Language spec mirror OK."

# Regenerate the LLM trigger quickref from the live ProviderCatalog metadata.
gen-trigger-quickref:
	$(HARN_CLI_CMD) dump-trigger-quickref

# CI guard: fail if the trigger quickref is stale relative to ProviderCatalog.
check-trigger-quickref:
	@echo "=== Checking docs/llm/harn-triggers-quickref.md is up to date ==="
	@$(HARN_CLI_CMD) dump-trigger-quickref --check
	@echo "    Harn trigger quickref OK."

# Regenerate the provider/model capability matrix from capabilities.toml.
gen-provider-matrix:
	@$(HARN_BIN_ASSIGN); \
	"$$harn_bin" provider catalog generate; \
	"$$harn_bin" provider catalog matrix

# CI guard: fail if the provider matrix docs drift from capabilities.toml.
check-provider-matrix:
	@echo "=== Checking docs/src/provider-matrix.md is up to date ==="
	@$(HARN_BIN_ASSIGN); \
	"$$harn_bin" provider catalog generate --check; \
	"$$harn_bin" provider catalog matrix --check
	@echo "    Harn provider matrix OK."

# Regenerate provider support recommendations from catalog/capabilities/notes.
gen-provider-support:
	@$(HARN_BIN_ASSIGN); \
	"$$harn_bin" provider catalog generate; \
	"$$harn_bin" provider catalog support

# CI guard: fail if provider support markdown or JSON drift.
check-provider-support:
	@echo "=== Checking provider support artifacts are up to date ==="
	@$(HARN_BIN_ASSIGN); \
	"$$harn_bin" provider catalog generate --check; \
	"$$harn_bin" provider catalog support --check
	@echo "    Harn provider support OK."

# Regenerate the checked-in provider/model catalog JSON, schema, and downstream bindings.
gen-provider-catalog:
	$(HARN_CMD) provider catalog generate

# CI guard: fail if checked-in provider/model catalog artifacts drift.
check-provider-catalog:
	@echo "=== Checking provider catalog artifacts ==="
	@$(HARN_CMD) provider catalog generate --check
	@echo "    Harn provider catalog artifacts OK."

# Regenerate the connector capability parity matrix from package manifests.
gen-connector-matrix:
	$(HARN_CLI_CMD) dump-connector-matrix

# CI guard: fail if the connector parity docs drift from package manifests.
check-connector-matrix:
	@echo "=== Checking docs/src/connectors/parity-matrix.md is up to date ==="
	@$(HARN_CLI_CMD) dump-connector-matrix --check
	@echo "    Harn connector matrix OK."

# CI guard: replay the provider catalog refresh workflow against bundled
# HTTP fixtures and verify the rendered drift report + candidate TOML
# match the committed goldens. After intentional adapter or fixture
# changes, run
#   $(HARN_CMD) run scripts/update_provider_catalog.harn -- --check --update
# and commit the regenerated files under scripts/provider_catalog_fixtures/.
check-provider-catalog-drift:
	@echo "=== Checking provider catalog refresh workflow ==="
	@$(HARN_CMD) run scripts/update_provider_catalog.harn -- --check
	@echo "    Provider catalog refresh OK."

# Validate the ready-to-customize trigger example library.
check-trigger-examples:
	@echo "=== Checking trigger examples ==="
	@find examples/triggers -mindepth 1 -maxdepth 1 -type d | sort | while IFS= read -r dir; do \
		test -f "$$dir/harn.toml"; \
		test -f "$$dir/lib.harn"; \
		test -f "$$dir/README.md"; \
		test -f "$$dir/SKILL.md"; \
		$(HARN_CMD) check "$$dir/lib.harn"; \
	done
	@echo "    Trigger examples OK."

check-docs-model-refs:
	@echo "=== Checking docs model references track provider aliases ==="
	@$(HARN_CMD) run scripts/check_docs_model_refs.harn

# Regenerate the diagnostic-code catalog (markdown page + JSON sidecar)
# from the in-binary registry in crates/harn-parser/src/diagnostic_codes.rs.
# Run this whenever you add, rename, retire, or rewire a HARN-<CAT>-<NNN>
# code or its repair template.
sync-diagnostics-catalog:
	$(HARN_CMD) explain --catalog --format markdown > docs/src/diagnostics.md
	$(HARN_CMD) explain --catalog --format json > docs/diagnostics-catalog.json

# CI guard: fail if docs/src/diagnostics.md or docs/diagnostics-catalog.json
# drift from the in-binary registry. `make sync-diagnostics-catalog` fixes it.
check-diagnostics-catalog:
	@echo "=== Checking diagnostic-code catalog is up to date ==="
	@set -e; \
	tmp_md=$$(mktemp); \
	tmp_json=$$(mktemp); \
	trap 'rm -f "$$tmp_md" "$$tmp_json"' EXIT; \
	$(HARN_CMD) explain --catalog --format markdown > "$$tmp_md"; \
	$(HARN_CMD) explain --catalog --format json > "$$tmp_json"; \
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
# `harn parse`; blocks tagged ```harn,check must pass full `harn check`.
# Blocks tagged ```harn,ignore are skipped.
check-docs-snippets:
	@echo "=== Checking docs snippets parse under harn parse ==="
	@$(HARN_CMD) run scripts/check_docs_snippets.harn

# CI guard: every harn long flag in docs/src bash/sh blocks must exist in
# the corresponding `harn ... --help` output.
check-docs-cli-flags:
	@echo "=== Checking docs bash/sh harn flags against --help ==="
	@$(HARN_CMD) run scripts/check_docs_cli_flags.harn

# CI guard: every local Markdown/HTML href under docs/src resolves to a
# checked-in repository file. Fragments are ignored; this only catches dead
# file paths.
check-docs-links:
	@echo "=== Checking docs internal links ==="
	@$(HARN_CMD) run scripts/check_docs_links.harn

# CI guard: every checked-in Harn snippet used by website/src parses under
# `harn check`.
check-site-snippets:
	@echo "=== Checking site snippets parse under harn check ==="
	@$(HARN_CMD) run scripts/check_site_snippets.harn

# CI guard: the workflow-authoring quickstart fixtures still produce the
# bundle digests, executed-node sequences, and connector-status shapes
# the docs claim.
check-docs-workflow-quickstart:
	@$(HARN_CMD) run scripts/check_docs_workflow_quickstart.harn

# Lint test files for wall-clock polling patterns that cause flaky tests.
# See docs/src/dev/testing.md for approved alternatives and the opt-out mechanism.
lint-test-patterns:
	@$(HARN_CMD) run scripts/lint_test_patterns.harn

lint-diagnostic-codes:
	@$(HARN_CMD) run scripts/check_diagnostic_codes.harn

# CI ratchet: fail if any stdlib .harn field-accesses an unvalidated boundary
# value (HARN-OWN-004) outside the frontier exclusion list. See the script
# header for the narrowing idioms and how to shrink the frontier.
check-stdlib-strict-types:
	@./scripts/check_stdlib_strict_types.sh

# CI ratchet: fail if any new public stdlib `pub fn` omits an explicit return
# annotation (HARN-STD-102). Existing debt is tracked by an AST/linter-derived
# baseline so migration waves can shrink it deliberately.
check-stdlib-public-return-types:
	@./scripts/check_stdlib_public_return_types.sh

check-receipt-structs:
	@$(HARN_CMD) run scripts/check_receipt_struct_duplication.harn

lint-no-rust-prompt-prose:
	@./scripts/check_no_rust_prompt_prose.sh

lint-agent-path-normalization:
	@./scripts/check_agent_path_normalization.sh --self-test
	@./scripts/check_agent_path_normalization.sh

lint-no-xfail-regression:
	@$(HARN_CMD) run scripts/check_xfail_count.harn

# CI ratchet: fail if any Rust CLI handler listed in
# scripts/ported_handlers.toml grows past its budgeted LOC. Tracks
# epic #2293 (subticket #2314 = C1). See the script header for how to
# add new entries / adjust budgets.
check-ported-handler-loc:
	@$(HARN_CMD) run scripts/check_ported_handler_loc.harn

# Repo-wide 1500-line ceiling for Rust and stdlib Harn. Existing debt is
# grandfathered at exact per-source counts so unrelated refactors cannot
# conflict in a central baseline. Regeneration only tightens existing debt.
check-source-file-lengths:
	@$(HARN_CMD) run scripts/check_source_file_lengths.harn

update-source-file-length-baseline:
	@$(HARN_CMD) run scripts/check_source_file_lengths.harn -- --update

check-python-boundary:
	@$(HARN_CMD) run scripts/check_python_boundary.harn

check-harn-syntax-sensitive-scans:
	@$(HARN_CMD) run scripts/check_harn_syntax_sensitive_scans.harn

# Regenerate tree-sitter-harn/grammar/keywords.js from the lexer's
# KEYWORDS const (the source of truth for reserved words). Run this
# whenever a keyword is added, renamed, or retired.
gen-tree-sitter-keywords:
	@$(HARN_CMD) run scripts/sync_tree_sitter_keywords.harn -- --write

# CI guard: fail if the tree-sitter keyword list drifts from the lexer's
# KEYWORDS const, so the editor grammar and the runtime parser agree on
# the reserved-word set. `make gen-tree-sitter-keywords` fixes it.
check-tree-sitter-keywords:
	@echo "=== Checking tree-sitter keyword list matches the lexer ==="
	@$(HARN_CMD) run scripts/sync_tree_sitter_keywords.harn

# CI guard: fail if the spec grammar's keyword literals drift from the lexer's
# KEYWORDS const (a keyword renamed/removed in the lexer but left stale in the
# `## Grammar` section). Contextual keywords are allowlisted in the script.
check-grammar-keywords:
	@echo "=== Checking spec grammar keyword literals match the lexer ==="
	@$(HARN_CMD) run scripts/check_grammar_keywords.harn

# Meta-guard: fail if scripts/generated_artifacts.toml (the single source
# of truth for every gen/check drift pair) has drifted from its consumers
# -- the Makefile `all:` recipe, the CI workflows, and the declared output
# files. See the registry header for the add-a-new-artifact checklist.
check-generated-registry:
	@echo "=== Checking generated-artifact registry is in sync ==="
	@$(HARN_CMD) run scripts/check_generated_registry.harn

check-release-audit-contract:
	@echo "=== Checking release-audit proof contract against CI ==="
	@$(HARN_CMD) run scripts/release_audit_contract.harn -- --contract scripts/release_audit_contract.json --check-ci .github/workflows/ci.yml

check-ci-cache-policy:
	@echo "=== Checking CI cache ownership policy ==="
	@$(HARN_CMD) run scripts/check_ci_cache_policy.harn
