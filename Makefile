.PHONY: setup setup-rust setup-bootstrap clean-stale-targets install-hooks configure-merge-drivers build build-harn build-release sign-local check fmt fmt-app-host fmt-harn fmt-harn-fix lint lint-md lint-actions lint-actions-source lint-actions-harn lint-harn check-app-host spec-lint gen-openapi-snapshot check-openapi-snapshot test test-focused test-one test-e2e test-cargo test-fast test-harn-scripts test-agent-scripts test-pr-gate-scripts conformance mechanism-contracts protocol-conformance mcp-conformance replay-oracle replay-bench eval-tool-calls bench bench-vm bench-vm-micro bench-vm-clone check-vm-rss-soak check-test-case-performance bench-llm bench-orchestration bench-cli-cold-start loadgen-postgres all release-gate release-smoke smoke-audit portal portal-check portal-demo gen-cli-aot check-cli-aot gen-highlight check-highlight gen-prompt-grammar check-prompt-grammar gen-protocol-artifacts check-protocol-artifacts gen-connector-schemas check-connector-schemas gen-harness-migrations check-harness-migrations check-downstream-protocol-artifacts check-bindings gen-session-bundle-schema check-session-bundle-schema gen-run-view-fixtures check-run-view-fixtures gen-trigger-quickref check-trigger-quickref gen-provider-matrix check-provider-matrix check-provider-support check-provider-catalog check-connector-matrix check-trigger-examples check-docs-model-refs check-docs-snippets check-docs-symbols check-docs-cli-flags check-docs-links check-site-snippets check-docs-workflow-quickstart sync-language-spec check-language-spec sync-diagnostics-catalog check-diagnostics-catalog lint-test-patterns lint-diagnostic-codes check-stdlib-host-neutral check-public-product-names check-stdlib-strict-types check-stdlib-public-return-types check-schema-strict check-optional-dep-feature-contracts check-receipt-structs lint-no-rust-prompt-prose lint-agent-path-normalization lint-no-xfail-regression check-provider-catalog-drift check-ported-handler-loc check-source-file-lengths check-python-boundary check-harn-syntax-sensitive-scans check-agent-guidance check-crate-sibling-versions check-protocol-symbol-removals check-dependabot-groups gen-tree-sitter-keywords check-tree-sitter-keywords gen-tree-sitter-parser check-tree-sitter-parser check-grammar-keywords gen-grammar-fitness check-grammar-fitness check-loud-boundaries check-turn-end-boundary check-generated-registry check-release-audit-contract check-ci-cache-policy check-rust-test-lane-policy check-cargo-lock-contract gen-vm-exposures check-vm-exposures check-binary-size-policy check-all-features
.PHONY: test-pr-gate-post-warm-integrations test-rust-lint-lane-cache
.PHONY: check-docs check-docs-portable check-docs-exact check-docs-cookbook-entrypoints
.PHONY: check-typescript-protocol-binding check-swift-protocol-binding
.PHONY: sync-docs-diagnostics
.PHONY: setup-wasm setup-wasm-tools gen-wasm-wit check-wasm-wit wasm-build gen-app-runtime check-app-runtime wasm-audit-imports wasm-test-browser wasm-check wasm-demo kernel-check kernel-test kernel-vm-parity vm-check cli-check cli-test gen-portable-benchmark-schema check-portable-benchmark-schema gen-portable-demo-package check-portable-demo-package

HARN_BIN ?=
PROTOCOL_ARTIFACT_VERSION ?=
HARN_CONFORMANCE_TIMEOUT_MS ?= 60000
HARN_CARGO_CMD = ./scripts/cargo_with_worktree_build_dir.sh
# Rust tests start from a known security-policy environment. Focused tests may
# still seed these variables explicitly after process startup. Harn script
# tests use harn_test_env.sh so they also get a fresh durable session store.
HARN_EGRESS_TEST_ENV = env -u HARN_EGRESS_ALLOW -u HARN_EGRESS_DENY -u HARN_EGRESS_DEFAULT -u HARN_EGRESS_BLOCK_PRIVATE -u HARN_EGRESS_ALLOW_LOOPBACK
HARN_RUST_TEST_ENV = $(HARN_EGRESS_TEST_ENV) HARN_LLM_CALLS_DISABLED=1 RUST_MIN_STACK="$${RUST_MIN_STACK:-16777216}"
HARN_SCRIPT_TEST_ENV = bash ./scripts/harn_test_env.sh
HARN_BIN_CMD = ./scripts/harn_bin.sh
HARN_BIN_PRINT_CMD = $(if $(strip $(HARN_BIN)),env HARN_BIN="$(HARN_BIN)" $(HARN_BIN_CMD) --print,$(HARN_BIN_CMD) --print)
HARN_CMD = $(if $(strip $(HARN_BIN)),env HARN_BIN="$(HARN_BIN)" $(HARN_BIN_CMD) --,$(HARN_BIN_CMD) --)
HARN_NO_BUILD_CMD = $(if $(strip $(HARN_BIN)),env HARN_BIN="$(HARN_BIN)" $(HARN_BIN_CMD) --no-build --,$(HARN_BIN_CMD) --no-build --)
HARN_CMD_VERBOSE = $(HARN_CMD)
HARN_CLI_CMD = $(HARN_CMD)
HARN_BIN_ASSIGN = harn_bin="$$($(HARN_BIN_PRINT_CMD))"
PROTOCOL_ARTIFACT_CHECK_ARGS = $(if $(strip $(PROTOCOL_ARTIFACT_VERSION)),--artifact-version "$(PROTOCOL_ARTIFACT_VERSION)" --check,--check)
HARN_WASM_PACK_VERSION ?= 0.15.0
HARN_WASM_PACK_DIR ?= $(or $(TMPDIR),/tmp)/harn-tools/wasm-pack-$(HARN_WASM_PACK_VERSION)
HARN_WASM_PACK = $(HARN_WASM_PACK_DIR)/bin/wasm-pack
HARN_WASM_TOOLS_VERSION ?= 1.255.0
HARN_WASM_COMPONENT_TOOLS_DIR ?= $(or $(TMPDIR),/tmp)/harn-tools/wasm-tools-$(HARN_WASM_TOOLS_VERSION)
HARN_WASM_TOOLS ?= $(HARN_WASM_COMPONENT_TOOLS_DIR)/bin/wasm-tools
HARN_WASM_OUT_DIR ?= pkg
HARN_WASM_BINARY ?= crates/harn-wasm/$(HARN_WASM_OUT_DIR)/harn_wasm_bg.wasm
HARN_APP_RUNTIME_WASM ?= crates/harn-cli/src/commands/app_host/runtime/harn_wasm_bg.wasm.gz
# Panic locations embed absolute paths from the builder's Cargo registry and
# toolchain sysroot, and this module ships to users inside the CLI. Without
# remapping, whoever regenerates the runtime publishes their home directory and
# host triple in it. Map both roots to fixed names so the artifact says nothing
# about the machine that produced it. RUSTFLAGS is set rather than appended so
# the flags do not vary with an inherited value (CI exports one).
HARN_CARGO_HOME ?= $(or $(CARGO_HOME),$(HOME)/.cargo)
HARN_RUST_SYSROOT ?= $(shell rustc --print sysroot)
HARN_WASM_RUSTFLAGS ?= --remap-path-prefix=$(HARN_CARGO_HOME)/registry=/cargo-registry --remap-path-prefix=$(HARN_RUST_SYSROOT)=/rust-toolchain
HARN_CHROMEDRIVER ?=

# Full quality check: format first, then lint/test in parallel.
# Usage: make all -j       (parallel checks after formatting)
#        make all           (sequential, also works)
all: fmt
	@$(HARN_BIN_ASSIGN); \
	build_freshness_id="$${HARN_BUILD_FRESHNESS_ID:-}"; \
	if [ -z "$$build_freshness_id" ] && [ -z "$(strip $(HARN_BIN))" ]; then \
		build_freshness_id="$$(HARN_BIN="$$harn_bin" ./scripts/harn_bin.sh --print-build-freshness)" || exit 1; \
	fi; \
	if [ -n "$$build_freshness_id" ]; then export HARN_BUILD_FRESHNESS_ID="$$build_freshness_id"; else unset HARN_BUILD_FRESHNESS_ID; fi; \
	stable_root="$$(mktemp -d "$${TMPDIR:-/tmp}/harn-all-bin.XXXXXX")" || exit 1; \
	trap 'rm -rf "$$stable_root"' EXIT; \
	harn_bin="$$(./scripts/snapshot_harn_bin.sh "$$harn_bin" "$$stable_root/harn-bin")" || exit 1; \
	$(MAKE) HARN_BIN="$$harn_bin" lint lint-md lint-actions lint-harn check-app-host spec-lint check-openapi-snapshot fmt-harn test test-harn-scripts test-agent-scripts test-pr-gate-scripts test-rust-lint-lane-cache conformance protocol-conformance mcp-conformance replay-oracle replay-bench check-highlight check-portable-benchmark-schema check-portable-demo-package check-prompt-grammar check-protocol-artifacts check-connector-schemas check-harness-migrations check-bindings check-session-bundle-schema check-run-view-fixtures check-docs lint-test-patterns lint-diagnostic-codes check-stdlib-host-neutral check-public-product-names check-stdlib-strict-types check-stdlib-public-return-types check-schema-strict check-optional-dep-feature-contracts check-receipt-structs check-provider-catalog-drift check-source-file-lengths check-python-boundary check-harn-syntax-sensitive-scans check-agent-guidance check-crate-sibling-versions check-protocol-symbol-removals check-dependabot-groups check-tree-sitter-keywords check-tree-sitter-parser check-grammar-keywords check-grammar-fitness check-loud-boundaries check-turn-end-boundary check-release-audit-contract check-ci-cache-policy check-rust-test-lane-policy check-cargo-lock-contract check-vm-exposures portal-check || exit 1; \
	if [ -z "$(strip $(HARN_BIN))" ]; then HARN_BIN='' HARN_BIN_NO_BUILD=1 ./scripts/harn_bin.sh --record-receipt; fi

check: all

setup:
	./scripts/dev_setup.sh

# Focused Rust setup for remote or constrained machines. It configures local
# build paths and warms the canonical linked Harn graph without installing
# optional tools or frontend dependencies.
setup-rust:
	HARN_DEV_SETUP_PROFILE=rust HARN_DEV_TARGET_WORKTREE_PATH="$(CURDIR)" ./scripts/dev_setup.sh

# Configure an agent-created staging worktree without compiling it. End-to-end
# task workflows may move into a final isolated lane; that lane should own the
# single setup build instead of compiling both checkouts.
setup-bootstrap:
	HARN_DEV_SETUP_PROFILE=bootstrap HARN_DEV_TARGET_WORKTREE_PATH="$(CURDIR)" ./scripts/dev_setup.sh

# Install the pinned browser-Wasm toolchain outside the checkout so every
# worktree keeps its mutable build products private while reusing one immutable
# tool binary. wasm-pack installs the matching wasm-bindgen test runner.
setup-wasm:
	rustup target add wasm32-unknown-unknown
	@if [ ! -x "$(HARN_WASM_PACK)" ] || [ "$$($(HARN_WASM_PACK) --version)" != "wasm-pack $(HARN_WASM_PACK_VERSION)" ]; then \
		cargo install --locked --root "$(HARN_WASM_PACK_DIR)" --version "=$(HARN_WASM_PACK_VERSION)" wasm-pack; \
	fi

setup-wasm-tools:
	@if [ ! -x "$(HARN_WASM_TOOLS)" ] || [ "$$($(HARN_WASM_TOOLS) --version)" != "wasm-tools $(HARN_WASM_TOOLS_VERSION)" ]; then \
		cargo install --locked --root "$(HARN_WASM_COMPONENT_TOOLS_DIR)" --version "=$(HARN_WASM_TOOLS_VERSION)" wasm-tools; \
	fi

# WIT is the stable Component Model contract even while browsers consume the
# core-Wasm wasm-bindgen Adapter. wasm-tools is pinned in the browser CI lane;
# override HARN_WASM_TOOLS locally when it is outside PATH.
gen-wasm-wit: setup-wasm-tools
	@command -v "$(HARN_WASM_TOOLS)" >/dev/null || { echo "wasm-tools is required (CI pins 1.255.0)" >&2; exit 1; }
	"$(HARN_WASM_TOOLS)" component wit crates/harn-wasm/wit --json > crates/harn-wasm/wit/harn-kernel.json

check-wasm-wit: setup-wasm-tools
	@command -v "$(HARN_WASM_TOOLS)" >/dev/null || { echo "wasm-tools is required (CI pins 1.255.0)" >&2; exit 1; }
	@actual="$$(mktemp)"; trap 'rm -f "$$actual"' EXIT; \
		"$(HARN_WASM_TOOLS)" component wit crates/harn-wasm/wit --json > "$$actual"; \
		diff -u crates/harn-wasm/wit/harn-kernel.json "$$actual"

# Core-Wasm ES modules are the immediate browser Adapter. Override
# HARN_WASM_OUT_DIR for a disposable baseline or a packaging lane.
wasm-build: setup-wasm
	cd crates/harn-wasm && RUSTFLAGS="$(HARN_WASM_RUSTFLAGS)" "$(HARN_WASM_PACK)" build --target web --release --out-dir "$(HARN_WASM_OUT_DIR)"

# The standalone Apps host embeds one compressed copy of the same Wasm adapter
# tested below. Generated JavaScript remains readable and type checked; Rust
# only serves the immutable files with the correct browser headers.
gen-app-runtime: wasm-build
	node scripts/package_app_runtime.mjs

check-app-runtime: wasm-build
	node scripts/package_app_runtime.mjs --check

# Core Wasm must import only the tiny generated wasm-bindgen exception Adapter.
# Any other host import is an authority change and fails closed for review.
# Audit both the module CI just built and the compressed copy the CLI ships,
# so a committed runtime cannot carry authority the fresh build would reject.
wasm-audit-imports: wasm-build
	node scripts/check_wasm_imports.mjs "$(HARN_WASM_BINARY)"
	node scripts/check_wasm_imports.mjs "$(HARN_APP_RUNTIME_WASM)"

# Browser tests are intentionally a real dedicated-worker path, not Node.
wasm-test-browser: setup-wasm
	@driver="$${HARN_CHROMEDRIVER:-$$(./scripts/resolve_chromedriver.sh)}"; \
	cd crates/harn-wasm && "$(HARN_WASM_PACK)" test --headless --chrome --chromedriver "$$driver"

wasm-check: check-wasm-wit wasm-build check-app-runtime wasm-audit-imports wasm-test-browser

# The browser demo consumes this generated package manifest, while the native
# benchmark consumes the same Harn source files directly. Keep the linker
# projection mechanical so the two adapters cannot drift in their sample app.
gen-portable-demo-package:
	$(HARN_CLI_CMD) portable package crates/harn-wasm/demo/package-root.harn --output crates/harn-wasm/demo/package.json

check-portable-demo-package:
	@echo "=== Checking Portable Harn demo package projection ==="
	@$(HARN_CLI_CMD) portable package crates/harn-wasm/demo/package-root.harn --output crates/harn-wasm/demo/package.json --check
	@echo "    Portable demo package OK."

# Serve the standalone reducer with correct Wasm MIME types on every Node
# platform. The worker remains the execution owner; this process only serves
# immutable files from crates/harn-wasm.
wasm-demo: wasm-build
	node scripts/serve_wasm_demo.mjs

kernel-check:
	$(HARN_CARGO_CMD) check -p harn-kernel

kernel-test:
	$(HARN_CARGO_CMD) nextest run -p harn-kernel $(KERNEL_TEST_ARGS)

# The corpus is included from harn-kernel/testdata by both this native-VM test
# and the browser-Wasm tests. It must stay one data source with three adapters.
kernel-vm-parity:
	$(HARN_RUST_TEST_ENV) $(HARN_CARGO_CMD) nextest run -p harn-vm --test portable_kernel_parity

vm-check:
	$(HARN_CARGO_CMD) check -p harn-vm

# Focused CLI wrappers keep local iteration off the full workspace gate. Pass
# native nextest filters through CLI_TEST_ARGS when only one surface changed.
cli-check:
	$(HARN_CARGO_CMD) check -p harn-cli

cli-test:
	$(HARN_RUST_TEST_ENV) $(HARN_CARGO_CMD) nextest run -p harn-cli $(CLI_TEST_ARGS)

# Reclaim orphaned per-worktree Cargo target dirs under known setup storage
# roots (left behind when an agent/codex worktree is removed). Add
# --dry-run to preview: make clean-stale-targets ARGS=--dry-run
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
	@HARN_BIN='' HARN_BIN_NO_BUILD=0 ./scripts/harn_bin.sh --print >/dev/null
	@HARN_LOCAL_SIGN_QUIET=1 ./scripts/sign_local_macos.sh
	@HARN_BIN='' HARN_BIN_NO_BUILD=0 ./scripts/harn_bin.sh --record-receipt

# Focused canonical CLI build for product-path iteration. This retains the
# worktree target isolation and signing contract without compiling unrelated
# workspace binaries.
build-harn:
	$(HARN_CARGO_CMD) build -p harn-cli --bin harn
	@HARN_BIN='' HARN_BIN_NO_BUILD=0 ./scripts/harn_bin.sh --print >/dev/null
	@HARN_LOCAL_SIGN_QUIET=1 ./scripts/sign_local_macos.sh
	@HARN_BIN='' HARN_BIN_NO_BUILD=0 ./scripts/harn_bin.sh --record-receipt

build-release:
	$(HARN_CARGO_CMD) build --release
	@HARN_LOCAL_SIGN_QUIET=1 ./scripts/sign_local_macos.sh

# Re-sign already-built harn binaries without rebuilding. Useful after
# pulling, switching worktrees, or any path that touched target/ without
# going through `make build` (e.g. `cargo run` with sccache).
sign-local:
	@HARN_BIN='' HARN_BIN_NO_BUILD=0 ./scripts/harn_bin.sh --print >/dev/null
	./scripts/sign_local_macos.sh
	@HARN_BIN='' HARN_BIN_NO_BUILD=0 ./scripts/harn_bin.sh --record-receipt

# Format all code
fmt: fmt-app-host
	$(HARN_CARGO_CMD) fmt --all

# Keep the embedded browser host readable. TypeScript checks the JavaScript in
# place; there is no generated copy and no frontend build at runtime.
fmt-app-host:
	@if command -v npm >/dev/null 2>&1; then \
		npm run app-host:format; \
	elif [ "$${CI:-}" = "true" ]; then \
		echo "npm is required to format app-host sources in CI" >&2; \
		exit 1; \
	else \
		echo "warning: npm not found; skipping app-host formatting" >&2; \
	fi

check-app-host:
	npm run app-host:check

# Run clippy lints (deny warnings in CI)
lint: lint-no-rust-prompt-prose lint-no-xfail-regression
	$(HARN_CARGO_CMD) clippy --workspace --all-targets -- -D warnings
	$(HARN_CARGO_CMD) clippy -p harn-cli --bin harn-freshness-check \
		--features internal-freshness-checker -- -D warnings

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

check-protocol-symbol-removals:
	@echo "=== Checking no published protocol symbol was removed undeclared ==="
	$(HARN_CMD) run scripts/check_protocol_symbol_removals.harn

check-dependabot-groups:
	$(HARN_CMD) run scripts/check_dependabot_groups.harn

# Run the fast (in-process, deterministic) test suite via cargo-nextest.
# Subprocess-spawning integration tests are excluded by the nextest "default"
# profile's default-filter. Run `make test-e2e` for the slow E2E suite.
#
# Requires cargo-nextest. Do not silently fall back to `cargo test`: that
# runner has no profile filter (so it widens into the e2e tier) and runs tests
# as threads in one process (so process-local env mutations race). Use
# `make test-cargo` only when you intentionally want those semantics (#6145).
# `ARGS` replaces the default `--workspace` selector so `-p <crate>` also
# narrows compilation instead of forming Cargo's additive workspace union.
test:
	@$(HARN_CARGO_CMD) nextest --version >/dev/null 2>&1 || { \
		echo "make test requires cargo-nextest; run 'make setup' or 'cargo install cargo-nextest --locked'" >&2; \
		echo "for intentional plain cargo test (different isolation; includes e2e binaries): make test-cargo" >&2; \
		exit 1; }
	$(HARN_RUST_TEST_ENV) $(HARN_CARGO_CMD) nextest run $(if $(strip $(ARGS)),$(ARGS),--workspace)

# Run a native nextest expression without passing it through Make or a shell
# command line. Optional package and integration-binary selectors are consumed
# by the same typed environment boundary.
test-focused:
	$(HARN_RUST_TEST_ENV) ./scripts/test_focused.sh

# Run exactly one Rust test without making nextest enumerate every test binary
# in the package first. The environment-variable boundary keeps the
# fully-qualified test name opaque to Make and the shell parser.
#
# The target kind is part of the request, because a test name is reachable only
# through the target that defines it. Leave HARN_TEST_ONE_BINARY unset for a
# library test under the package's src/; set it to the owning integration-test
# binary for a test under the package's tests/. The runner refuses a name the
# requested target does not define rather than filtering to zero matches.
test-one:
	@ : "$${HARN_TEST_ONE_NAME:?set HARN_TEST_ONE_NAME to the fully-qualified test name}"
	@if [ -n "$${HARN_TEST_ONE_BINARY:-}" ]; then \
		$(HARN_RUST_TEST_ENV) ./scripts/test_one.sh \
			--package "$${HARN_TEST_ONE_PACKAGE:-harn-cli}" \
			--test "$${HARN_TEST_ONE_BINARY}" "$${HARN_TEST_ONE_NAME}"; \
	else \
		$(HARN_RUST_TEST_ENV) ./scripts/test_one.sh \
			--package "$${HARN_TEST_ONE_PACKAGE:-harn-cli}" \
			--lib "$${HARN_TEST_ONE_NAME}"; \
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
	@$(HARN_CARGO_CMD) nextest --version >/dev/null 2>&1 || { \
		echo "test-affected requires cargo-nextest; run 'make setup'"; exit 1; }
	@args="$$(./scripts/ci/affected_crate_args.sh --base "$(AFFECTED_BASE)")"; \
	if [ -z "$$args" ]; then \
		echo "make test-affected: no affected crates; skipping Rust tests."; \
		exit 0; \
	fi; \
	echo "make test-affected: cargo nextest run $$args"; \
	$(HARN_RUST_TEST_ENV) $(HARN_CARGO_CMD) nextest run $$args

# Run the slow E2E / smoke suite: subprocess-spawning CLI surface tests,
# signal handling, MCP server launch, real ProcessHandle smoke tests, etc.
# Runs on schedule (nightly), manually, and on PRs with the `e2e` label.
# Requires cargo-nextest (no plain `cargo test` fallback for profile support).
test-e2e:
	$(HARN_RUST_TEST_ENV) $(HARN_CARGO_CMD) nextest run --workspace --profile e2e --run-ignored all

# Run the baseline Cargo workspace test command explicitly.
test-cargo:
	$(HARN_RUST_TEST_ENV) $(HARN_CARGO_CMD) test --workspace

# Compatibility alias for the smarter default `make test`.
test-fast:
	@$(MAKE) test

# Run Harn conformance test suite
conformance:
	$(HARN_SCRIPT_TEST_ENV) $(HARN_CMD_VERBOSE) test conformance --parallel $(if $(HARN_CONFORMANCE_JOBS),--jobs $(HARN_CONFORMANCE_JOBS)) --timeout $(HARN_CONFORMANCE_TIMEOUT_MS)

# Mechanism-contract onramp tier: the manufactured mini-evals that prove a new
# termination/escalation/judge/guard/routing mechanism ENGAGES correctly (fires
# on its trigger, emits its effect, does NOT fire on the negative case) before
# any N>=5 convergence gauntlet. A focused, fast filter over the
# conformance/tests/mechanisms/*.contract.harn suite — already covered by
# `make conformance`, broken out here for authoring and as the documented gate.
# See conformance/tests/mechanisms/README.md.
mechanism-contracts:
	$(HARN_SCRIPT_TEST_ENV) $(HARN_CMD_VERBOSE) test conformance --filter '.contract'

protocol-conformance:
	$(HARN_SCRIPT_TEST_ENV) $(HARN_CMD_VERBOSE) test protocols

# MCP stable compatibility harness: exercises Harn's MCP client against fake
# stable servers, fake stable clients against the generic and orchestrator
# servers, and validates the published wire fixtures + JSON Schema
# 2020-12 recursive `$defs` handling.
#
# Failures are scoped per surface so CI breakage attribution is
# unambiguous:
#   - tests/harn_mcp_compat/client.rs         — fake-server self-consistency
#   - tests/harn_mcp_compat/generic_server.rs — generic harn-serve MCP server
#   - tests/harn_mcp_compat/artifacts.rs      — published fixtures + recursive $defs
#   - harn-cli mcp_compat_tests — orchestrator MCP server
#
# This target is a developer-convenience entry point for local iteration
# on the MCP surface — it lets you re-run the focused suite without
# building the whole workspace. CI does NOT call it: the two `cargo test`
# invocations below are a strict subset of `cargo nextest run --workspace`
# in the `rust-test` workflow job, so running it again in `harn-audit`
# would pay ~4 min of wall-clock to redo work the workspace test run
# already covers.
mcp-conformance:
	@echo "=== MCP stable harness: harn-mcp-compat suite (client / generic_server / artifacts) ==="
	$(HARN_RUST_TEST_ENV) $(HARN_CARGO_CMD) test -p harn-mcp-compat --tests
	@echo "=== MCP stable harness: orchestrator server (harn-cli mcp_compat_tests) ==="
	$(HARN_RUST_TEST_ENV) $(HARN_CARGO_CMD) test -p harn-cli --lib mcp_compat_tests

replay-oracle:
	$(HARN_SCRIPT_TEST_ENV) $(HARN_CMD_VERBOSE) orchestrator replay-oracle

replay-bench:
	$(HARN_SCRIPT_TEST_ENV) $(HARN_CMD_VERBOSE) bench replay --json >/dev/null

eval-tool-calls:
	$(HARN_CMD_VERBOSE) eval tool-calls --dataset conformance/tool-call-eval --planner mock:mock --output .harn-runs/tool-call-eval/latest

bench: bench-vm

bench-vm:
	./scripts/bench_vm.sh

bench-vm-micro:
	./scripts/bench_vm_micro.sh

bench-vm-clone:
	cargo bench -p harn-vm-perf --bench bench_vmenv_clone -- --output-format bencher

check-vm-rss-soak:
	@$(HARN_BIN_ASSIGN); HARN_CHECK_BIN="$$harn_bin" $(HARN_CMD) run scripts/check_vm_rss_soak.harn

check-test-case-performance:
	@$(HARN_BIN_ASSIGN); HARN_CHECK_BIN="$$harn_bin" $(HARN_CMD) run scripts/check_test_case_performance.harn

bench-llm:
	cargo bench -p harn-llm-perf --bench bench_llm_options_roundtrip -- --output-format bencher

bench-orchestration:
	cargo bench -p harn-orchestration-perf --bench bench_hook_dispatch -- --output-format bencher

bench-cli-cold-start:
	./scripts/bench_cli_cold_start.sh

# Postgres hostlib loadgen. Self-skips (exit 0) when HARN_TEST_POSTGRES_URL
# is unset; see bench/postgres/README.md for the tunable env vars.
loadgen-postgres:
	@if [ -z "$${HARN_TEST_POSTGRES_URL:-}" ]; then \
		echo "harn-postgres-loadgen: HARN_TEST_POSTGRES_URL not set — skipping (no Postgres to drive)"; \
	else \
		cargo run --release -p harn-postgres-perf --bin harn-postgres-loadgen; \
	fi

# Lint markdown files
lint-md:
	npx markdownlint-cli2 "**/*.md"

# Lint the Harn Agents Protocol OpenAPI source contract with Redocly. The
# generated public path/schema snapshot (spec/openapi.snapshot) and the
# embedded server copy (crates/harn-serve/openapi.yaml) are guarded separately
# by `check-openapi-snapshot`, registered in scripts/generated_artifacts.toml.
spec-lint:
	./node_modules/.bin/redocly lint spec/openapi.yaml

# Regenerate the OpenAPI public-surface snapshot and the embedded server copy
# from spec/openapi.yaml after an intentional surface change.
gen-openapi-snapshot:
	$(HARN_CMD) run scripts/check_openapi_snapshot.harn -- --update

# Drift guard: fail if spec/openapi.snapshot or the embedded
# crates/harn-serve/openapi.yaml copy no longer matches spec/openapi.yaml.
check-openapi-snapshot:
	@echo "=== Checking OpenAPI surface snapshot is up to date ==="
	@$(HARN_CMD) run scripts/check_openapi_snapshot.harn
	@echo "    OpenAPI snapshot OK."

# Lint GitHub Actions workflows without requiring the Harn binary. Keep this
# target source-only so the dedicated hygiene job never pays for a Rust build.
lint-actions-source:
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

# Validate the Harn-specific runner-tier contract in a lane that already has a
# warm, exact-commit Harn binary.
lint-actions-harn:
	@# Keep each Blacksmith-capable job off `runner.environment` and its declared
	@# HARN_RUNNER_TIER in step with its runs-on ladder. Unlike actionlint above
	@# this is NOT allowed to skip: the failure it guards against is silent, so
	@# a gate that can silently not-run would be worthless against it.
	@$(HARN_CMD) run scripts/check_runner_tier.harn

lint-actions: lint-actions-source lint-actions-harn

# Reject unreviewed conformance diagnostics while preserving the explicitly
# triaged baseline. Paired .error/.lint fixtures own their diagnostics in the
# conformance runner and are excluded here.
lint-harn:
	@echo "=== Linting Harn conformance tests ==="
	@HARN_BIN="$$($(HARN_BIN_PRINT_CMD))" ./scripts/check-conformance-lint-baseline.sh
	@echo "=== Checking Harn experiment support modules ==="
	@$(HARN_CMD) check $(EXPERIMENT_HARN_CHECK)
	@echo "=== Linting Harn-authored scripts ==="
	@$(HARN_CMD) lint --strict scripts/*.harn scripts/tests/*.harn
# `lint --strict` does not typecheck: it reported no issues for scripts that
# `harn run` refuses to execute. Whether a script was typed came down to
# whether some other target happened to run that exact file, so a script only
# ever imported by a test had no type gate at all.
#
# `--strict-types` matches the bar harn-bump-fleet holds its own harnesses to
# in scripts/harn-project.sh. It adds HARN-OWN-004, which requires a parsed
# document to be validated at the boundary that reads it rather than
# dereferenced on faith.
	@echo "=== Type-checking Harn-authored scripts ==="
	@$(HARN_CMD) check --strict-types scripts/*.harn scripts/tests/*.harn
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
#
# Unparseable fixtures do not belong here: `harn fmt` reads the sibling `.error`
# declaration and skips them on its own, the same way `harn fix` does. This list
# held four such fixtures until that landed, and every name it carries is a name
# no consumer repo can know — which is how a repo-wide `harn fmt .` in the
# reusable bump workflow broke consumers that this gate stayed green on.
#
# What is left is the one case the declaration cannot express: a fixture that
# parses fine, but whose semicolon style the formatter would normalize away,
# erasing the thing it exists to test.
FMT_HARN_SKIP := semicolon_statements.harn
EXPERIMENT_HARN_CHECK := experiments/burin-mini/host.harn experiments/burin-mini/lib/common.harn experiments/burin-mini/lib/profiles.harn experiments/burin-mini/lib/workspace.harn
STDLIB_HARN_DIR := crates/harn-stdlib/src/stdlib
# Extra directories that contain user-facing .harn fixtures but were
# historically outside the fmt-harn gate. Keeping them in the
# gate avoids the "I edited persona X and pre-commit reformatted three
# unrelated files" surprise from accumulated drift.
EXTRA_HARN_DIRS := personas tests examples evals crates/harn-cli/assets/demo
EXTRA_HARN_FIND := find $(EXTRA_HARN_DIRS) -type d -name .harn -prune -o -type f -name '*.harn' -print0 2>/dev/null

fmt-harn-fix:
	@echo "=== Formatting Harn files ==="
	@find $(STDLIB_HARN_DIR) -type f -name '*.harn' -print0 \
		| xargs -0 $(HARN_CMD) fmt
	@find conformance/tests -type f -name '*.harn' $(foreach s,$(FMT_HARN_SKIP),-not -name $(s)) -print0 \
		| xargs -0 $(HARN_CMD) fmt
	@find experiments -type f -name '*.harn' -print0 \
		| xargs -0 $(HARN_CMD) fmt
	@find scripts -type d -name '.harn*' -prune -o -type f -name '*.harn' -print0 \
		| xargs -0 $(HARN_CMD) fmt
	@$(EXTRA_HARN_FIND) \
		| xargs -0 -r $(HARN_CMD) fmt
	@echo "    Harn formatting OK."

fmt-harn:
	@echo "=== Checking Harn formatting ==="
	@find $(STDLIB_HARN_DIR) -type f -name '*.harn' -print0 \
		| xargs -0 $(HARN_CMD) fmt --check
	@find conformance/tests -type f -name '*.harn' $(foreach s,$(FMT_HARN_SKIP),-not -name $(s)) -print0 \
		| xargs -0 $(HARN_CMD) fmt --check
	@find experiments -type f -name '*.harn' -print0 \
		| xargs -0 $(HARN_CMD) fmt --check
	@find scripts -type d -name '.harn*' -prune -o -type f -name '*.harn' -print0 \
		| xargs -0 $(HARN_CMD) fmt --check
	@$(EXTRA_HARN_FIND) \
		| xargs -0 -r $(HARN_CMD) fmt --check
	@echo "    Harn formatting OK."

# Base-aware semantic audit for formatter PRs that mechanically rewrite the
# Harn corpus. Override HARN_FMT_AUDIT_BASE when the target branch is not main.
audit-fmt-harn-tokens:
	HARN_FMT_AUDIT_BASE="$${HARN_FMT_AUDIT_BASE:-origin/main}" cargo test -p harn-fmt tests::semantic_tokens::merge_base_harn_rewrite_preserves_semantic_tokens -- --exact

# Run Harn-level interface tests for repository scripts and focused stdlib
# owners. Fixtures stay inside the test workspace except for the canonical
# spec mirror check. Wired into `make all` and exercised by CI.
test-harn-scripts:
	@echo "=== Running Harn script test suite ==="
	@$(HARN_SCRIPT_TEST_ENV) $(HARN_CMD) test scripts/tests/ --parallel
	@$(HARN_SCRIPT_TEST_ENV) $(HARN_CMD) test tests/stdlib/models_batch_rejoin_test.harn
	@$(HARN_SCRIPT_TEST_ENV) $(HARN_CMD) test tests/stdlib/polymorphic_param_widening_test.harn
	@$(HARN_SCRIPT_TEST_ENV) $(HARN_CMD) test tests/stdlib/runtime_content_fingerprint_test.harn
	@$(HARN_SCRIPT_TEST_ENV) $(HARN_CMD) test experiments/burin-mini/tests/ --parallel
	@$(HARN_SCRIPT_TEST_ENV) $(HARN_CMD) test experiments/diagnostics-timing/tests/ --parallel
	@echo "    Harn script tests OK."

check-binary-size-policy:
	@echo "=== Checking release binary-size policy ==="
	@$(HARN_SCRIPT_TEST_ENV) $(HARN_CMD) test scripts/tests/check_binary_size_test.harn
	@echo "    Release binary-size policy OK."

# Agent-loop Harn unit tests (stall detector, loop control, judge verdict,
# transcript helpers). These live outside `conformance/` so they are NOT walked
# by `make conformance`; this target wires them into CI so they cannot rot.
test-agent-scripts:
	@echo "=== Running Harn agent-loop test suite ==="
	@$(HARN_SCRIPT_TEST_ENV) $(HARN_CMD) test tests/agent/
	@echo "    Harn agent-loop tests OK."

test-pr-gate-scripts:
	./scripts/tests/check_stdlib_host_neutral_test.sh
	./scripts/tests/check_public_product_names_test.sh
	./scripts/tests/check_pr_metadata_privacy_test.sh
	./scripts/tests/check_issue_comment_privacy_test.sh
	./scripts/tests/apt_install_action_test.sh
	./scripts/tests/rust_toolchain_action_test.sh
	./scripts/tests/npm_ci_with_retry_test.sh
	./scripts/tests/ci_docs_only_test.sh
	./scripts/tests/ci_release_metadata_only_test.sh
	./scripts/tests/release_metadata_git_failure_test.sh
	./scripts/tests/tree_sitter_generated_test.sh
	./scripts/tests/native_platform_ci_plan_test.sh
	./scripts/tests/release_ref_matcher_test.sh
	./scripts/tests/ci_merge_group_proof_test.sh
	./scripts/tests/e2e_workflow_trigger_test.sh
	./scripts/tests/check_sdk_release_artifacts_test.sh
	./scripts/tests/generate_sdk_clients_test.sh
	./scripts/tests/changelog_fragment_check_test.sh
	./scripts/tests/release_pr_drift_check_test.sh
	./scripts/tests/release_ship_fragment_guard_test.sh
	./scripts/tests/release_tag_main_ancestry_test.sh
	./scripts/tests/verify_release_archive_provenance_test.sh
	./scripts/tests/check_linux_glibc_floor_test.sh
	./scripts/tests/release_version_test.sh
	./scripts/tests/release_publication_policy_test.sh
	./scripts/tests/prepare_development_version_test.sh
	./scripts/tests/development_bump_cutover_test.sh
	./scripts/tests/development_cutover_monitor_test.sh
	./scripts/tests/merge_group_path_gate_test.sh
	./scripts/tests/affected_crate_args_test.sh
	./scripts/tests/hook_fast_default_mode_test.sh
	./scripts/tests/hook_rust_gate_test.sh
	./scripts/tests/hook_timing_instrument_test.sh
	./scripts/tests/hook_registry_harn_bin_test.sh
	./scripts/tests/pre_push_validation_range_test.sh
	./scripts/tests/ci_rust_test_lane_test.sh
	./scripts/tests/thread_parity_receipt_test.sh
	./scripts/tests/macos_nightly_test_env_test.sh
	./scripts/tests/ci_finalize_sccache_test.sh
	./scripts/tests/sccache_action_cache_size_test.sh
	./scripts/tests/check_release_warm_build_budget_test.sh
	./scripts/tests/ci_wait_for_run_artifacts_test.sh
	./scripts/tests/ci_write_walltime_report_test.sh
	./scripts/tests/update_queued_pr_test.sh
	./scripts/tests/cancel_superseded_merge_groups_test.sh
	./scripts/tests/audit_gates_parallel_test.sh
	./scripts/tests/source_gate_receipt_test.sh
	./scripts/tests/conformance_worker_budget_test.sh
	./scripts/tests/rust_artifact_test.sh
	./scripts/tests/windows_workspace_warm_artifact_test.sh
	./scripts/tests/windows_storage_budget_test.sh
	./scripts/tests/ci_harn_bin_warm_test.sh
	./scripts/tests/harn_bin_resolver_test.sh
	./scripts/tests/harn_bin_recovery_batch_test.sh
	./scripts/tests/package_verify_bootstrap_test.sh
	./scripts/tests/verify_crate_dependency_resolution_test.sh
	./scripts/tests/harn_launcher_python_cutover_test.sh
	./scripts/tests/lint_harn_gate_test.sh
	./scripts/tests/build_revision_workflow_test.sh
	./scripts/tests/dev_setup_profile_test.sh
	./scripts/tests/sync_diagnostics_catalog_test.sh
	./scripts/tests/sign_local_macos_test.sh
	./scripts/tests/bench_vm_startup_test.sh
	./scripts/tests/cargo_build_dir_isolation_test.sh
	./scripts/tests/cargo_toolchain_pin_test.sh
	./scripts/tests/cargo_target_seed_test.sh
	./scripts/tests/cli_aot_merge_driver_test.sh
	./scripts/tests/release_gate_harn_bin_test.sh
	./scripts/tests/release_gate_stale_out_dir_test.sh
	./scripts/tests/prune_stale_targets_test.sh
	./scripts/tests/prune_stale_targets_retention_test.sh
	./scripts/tests/report_ci_cache_budget_test.sh
	./scripts/tests/loadgen_postgres_gate_test.sh
	./scripts/tests/check_all_features_test.sh
	./scripts/tests/check_stdlib_strict_types_test.sh
	./scripts/tests/test_focused_test.sh
	./scripts/tests/test_one_test.sh

# Rust/Harn-backed shell integration tests run only after CI restores the Rust
# toolchain/caches and exports the one warmed binary. Pure Harn semantics remain
# owned by test-harn-scripts, which discovers their @test fixtures exactly once.
test-rust-lint-lane-cache:
	./scripts/tests/rust_lint_lane_cache_test.sh

test-pr-gate-post-warm-integrations: test-rust-lint-lane-cache
	@if [ -z "$(strip $(HARN_BIN))" ] || [ ! -x "$(HARN_BIN)" ]; then \
		echo "test-pr-gate-post-warm-integrations requires an executable HARN_BIN" >&2; \
		exit 1; \
	fi
	HARN_BIN="$(HARN_BIN)" ./scripts/tests/cargo_target_seed_reuse_test.sh
	HARN_BIN="$(HARN_BIN)" ./scripts/tests/nextest_filters_from_paths_test.sh
	HARN_BIN="$(HARN_BIN)" ./scripts/tests/claude_dev_setup_once_test.sh
	HARN_BIN="$(HARN_BIN)" ./scripts/tests/publish_script_test.sh
	HARN_BIN="$(HARN_BIN)" ./scripts/tests/ci_preemption_recover_test.sh
	HARN_BIN="$(HARN_BIN)" ./scripts/tests/check_harn_syntax_sensitive_scans_performance_test.sh
	HARN_BIN="$(HARN_BIN)" ./scripts/tests/connector_scaffold_strict_package_test.sh
	HARN_BIN="$(HARN_BIN)" ./scripts/tests/drift_preflight_stale_binary_test.sh
	HARN_BIN="$(HARN_BIN)" ./scripts/tests/hook_generated_artifact_drift_warn_test.sh
	HARN_BIN_RESOLVER_TEST_ALLOW_CARGO=1 ./scripts/tests/harn_bin_resolver_test.sh
	HARN_BIN="$(HARN_BIN)" ./scripts/tests/agent_shell_guard_adapter_test.sh
	HARN_BIN="$(HARN_BIN)" ./scripts/tests/check_release_smoke_test.sh
	HARN_BIN="$(HARN_BIN)" ./scripts/tests/release_prepare_env_test.sh
	HARN_BIN="$(HARN_BIN)" ./scripts/tests/release_withdrawal_lineage_test.sh
	./scripts/tests/make_harn_cargo_env_test.sh
	./scripts/tests/embedded_asset_rebuild_test.sh

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
	target/release/harn run --no-sandbox scripts/release_smoke.harn -- --candidate target/release/harn

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
	"$$harn_binary" run --no-sandbox scripts/release_smoke.harn -- --candidate "$$harn_binary"

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

# Generate a target-independent CLI AOT payload for package/release assembly.
# The generator validates the full workspace; harn-cli's build script validates
# and embeds only the package-local payload. Source builds use source fallback.
HARN_CLI_AOT_GEN_CMD = $(if $(strip $(HARN_CLI_AOT_GEN_BIN)),"$(HARN_CLI_AOT_GEN_BIN)",$(HARN_CARGO_CMD) run -p harn-cli-aot-gen --)
HARN_CLI_AOT_ARTIFACT_VERSION_ARG = $(if $(strip $(HARN_CLI_AOT_ARTIFACT_VERSION)),--artifact-version "$(HARN_CLI_AOT_ARTIFACT_VERSION)",)

gen-cli-aot:
	$(HARN_CLI_AOT_GEN_CMD) --workspace-root "$(CURDIR)" $(HARN_CLI_AOT_ARTIFACT_VERSION_ARG)

check-cli-aot:
	@echo "=== Checking release/package CLI AOT payload ==="
	@$(HARN_CLI_AOT_GEN_CMD) --workspace-root "$(CURDIR)" $(HARN_CLI_AOT_ARTIFACT_VERSION_ARG) --check
	@echo "    CLI AOT payload OK."

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

# The Rust receipt type and its validation constants own this public schema.
gen-portable-benchmark-schema:
	$(HARN_CLI_CMD) dump-portable-benchmark-schema

check-portable-benchmark-schema:
	@echo "=== Checking Portable Kernel benchmark schema ==="
	@$(HARN_CLI_CMD) dump-portable-benchmark-schema --check
	@echo "    Portable Kernel benchmark schema OK."

# Regenerate the VS Code `.harn.prompt` TextMate grammar from the live
# prompt-template keyword, filter, and section vocabulary. Run this whenever
# crates/harn-vm/src/stdlib/template/vocabulary.rs changes.
gen-prompt-grammar:
	$(HARN_CLI_CMD) dump-prompt-grammar

# CI guard: fail if the editor grammar is stale relative to the prompt-template
# engine. `make gen-prompt-grammar` fixes it.
check-prompt-grammar:
	@echo "=== Checking the .harn.prompt editor grammar is up to date ==="
	@$(HARN_CLI_CMD) dump-prompt-grammar --check
	@echo "    Prompt grammar OK."

gen-protocol-artifacts:
	$(HARN_CLI_CMD) dump-protocol-artifacts

check-protocol-artifacts:
	@echo "=== Checking Harn protocol artifacts are up to date ==="
	@$(HARN_CLI_CMD) dump-protocol-artifacts $(PROTOCOL_ARTIFACT_CHECK_ARGS)
	@echo "    Harn protocol artifacts OK."

gen-connector-schemas:
	$(HARN_CLI_CMD) connector-schema-codegen

check-connector-schemas:
	@echo "=== Checking generated connector event schemas are up to date ==="
	@$(HARN_CLI_CMD) connector-schema-codegen --check
	@echo "    Connector event schemas OK."

gen-harness-migrations:
	$(HARN_CLI_CMD) dump-harness-migrations

# `harn-parser` compiles below `harn-vm`, so the type checker reads a generated
# projection of the runtime migration registry instead of a second hand-written
# table. Without this guard the two drift and `harn check` hands out renames the
# linter contradicts.
check-harness-migrations:
	@echo "=== Checking generated harness migration table is up to date ==="
	@$(HARN_CLI_CMD) dump-harness-migrations --check
	@echo "    Harness migration table OK."

check-downstream-protocol-artifacts:
	@echo "=== Checking downstream vendored protocol bindings match Harn ==="
	@$(HARN_CLI_CMD) run --no-sandbox scripts/check_downstream_protocol_bindings.harn -- --required
	@echo "    Downstream protocol bindings OK."

# Round-trip the published JSON fixture through generated protocol bindings and
# exercise the closed recap write contract before downstream consumers vendor
# the artifacts. Go remains optional for contributors, but CI requires it;
# Swift runs in the dedicated macOS lane below.
check-bindings:
	@echo "=== Checking Harn protocol bindings round-trip the published fixture ==="
	@$(HARN_CLI_CMD) run scripts/check_protocol_bindings.harn
	@$(MAKE) check-typescript-protocol-binding
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

check-typescript-protocol-binding:
	@set -eu; \
		tmp=$$(mktemp -d "$${TMPDIR:-/tmp}/harn-ts-binding.XXXXXX"); \
		trap 'rm -rf "$$tmp"' EXIT; \
		./node_modules/.bin/tsc --strict --module node16 --moduleResolution node16 --target es2022 --rootDir . --outDir "$$tmp" spec/protocol-artifacts/harn-protocol.ts spec/protocol-artifacts/harn-tools.ts scripts/tests/protocol_binding_session_recap.ts scripts/tests/tool_catalog_contract.ts; \
		node "$$tmp/scripts/tests/protocol_binding_session_recap.js" spec/protocol-artifacts/fixtures/round_trip.json; \
		node "$$tmp/scripts/tests/tool_catalog_contract.js"

check-swift-protocol-binding:
	@set -eu; \
		tmp=$$(mktemp -d "$${TMPDIR:-/tmp}/harn-swift-binding.XXXXXX"); \
		trap 'rm -rf "$$tmp"' EXIT; \
		xcrun swiftc -parse-as-library spec/protocol-artifacts/HarnProtocol.swift scripts/tests/protocol_binding_session_recap.swift -o "$$tmp/probe"; \
		"$$tmp/probe" spec/protocol-artifacts/fixtures/round_trip.json

gen-session-bundle-schema:
	$(HARN_CLI_CMD) session schema --out spec/schemas/session-bundle.v1.schema.json

check-session-bundle-schema:
	@echo "=== Checking session bundle schema is up to date ==="
	@$(HARN_CLI_CMD) session schema --check
	@echo "    Session bundle schema OK."

gen-run-view-fixtures:
	$(HARN_CLI_CMD) session view-fixtures --write --repository-root .

check-run-view-fixtures:
	@echo "=== Checking run/session view fixture snapshots ==="
	@$(HARN_CLI_CMD) session view-fixtures --check --repository-root .
	@echo "    Run/session view fixtures OK."

# Regenerate docs/src/language-spec.md from spec/HARN_SPEC.md (the
# canonical authoring source). Mirrors what release_gate.sh audit's
# sync_language_spec.harn step does.
sync-language-spec:
	$(HARN_NO_BUILD_CMD) run scripts/sync_language_spec.harn

# CI guard: fail if docs/src/language-spec.md is stale relative to
# spec/HARN_SPEC.md. `make sync-language-spec` fixes it.
check-language-spec:
	@echo "=== Checking docs/src/language-spec.md is up to date ==="
	@$(HARN_NO_BUILD_CMD) run scripts/sync_language_spec.harn -- --check
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
# The fixture workflow installs its own deterministic per-Harness egress policy. Clear
# operator/environment policy variables so that policy is not configured twice
# before the Harn script reaches its fixture setup.
check-provider-catalog-drift:
	@echo "=== Checking provider catalog refresh workflow ==="
	@$(HARN_SCRIPT_TEST_ENV) $(HARN_CMD) run --allow-process-network scripts/update_provider_catalog.harn -- --check
	@$(HARN_SCRIPT_TEST_ENV) $(HARN_CMD) test scripts/tests/provider_catalog_notice_test.harn
	@$(HARN_BIN_ASSIGN); HARN_BIN="$$harn_bin" ./scripts/tests/provider_catalog_notice_sandbox_test.sh
	@echo "    Provider catalog refresh OK."

# Validate the ready-to-customize trigger example library.
check-trigger-examples:
	@echo "=== Checking trigger examples ==="
	@set -e; for dir in examples/triggers/*; do \
		test -d "$$dir" || continue; \
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

# Docs checks split by the parser they need.
#
# The invariant: a check that parses a moving schema never runs a pinned
# parser. Everything below is sorted by which side of that line it falls on,
# because CI runs one half against a published release.

# Judgeable by any released binary. These read repository files and rule on
# prose, links, structure, and whether documented Harn still parses. The binary
# is an interpreter here, not the source of the answer, so a release a few
# commits behind reaches the same verdict the commit's own binary would.
check-docs-portable:
	+@$(HARN_BIN_ASSIGN); \
	case "$$harn_bin" in /*) ;; *) harn_bin="$(CURDIR)/$$harn_bin" ;; esac; \
	$(MAKE) --no-print-directory HARN_BIN="$$harn_bin" check-language-spec check-trigger-examples check-docs-model-refs check-docs-snippets check-docs-symbols check-docs-links check-docs-cookbook-entrypoints check-site-snippets check-docs-workflow-quickstart check-generated-registry

# Judgeable only by the commit's own binary. Each of these regenerates an
# artifact from data linked into the binary — the provider catalog, the trigger
# and connector tables, the diagnostic registry — or reads the binary's own
# `--help`, and compares that against what is checked in. The binary is the
# source of the expected value, so a different binary projects a different
# expectation and reports a tree that has moved on as unknown-fielded or stale.
# That is a fact about the two binaries, not about the tree.
check-docs-exact:
	+@$(HARN_BIN_ASSIGN); \
	case "$$harn_bin" in /*) ;; *) harn_bin="$(CURDIR)/$$harn_bin" ;; esac; \
	$(MAKE) --no-print-directory HARN_BIN="$$harn_bin" check-trigger-quickref check-provider-matrix check-provider-support check-provider-catalog check-connector-matrix check-diagnostics-catalog check-docs-cli-flags

check-docs:
	+@$(HARN_BIN_ASSIGN); \
	case "$$harn_bin" in /*) ;; *) harn_bin="$(CURDIR)/$$harn_bin" ;; esac; \
	$(MAKE) --no-print-directory HARN_BIN="$$harn_bin" check-docs-portable check-docs-exact

check-docs-cookbook-entrypoints:
	@echo "=== Checking cookbook entrypoints run without host-only task input ==="
	@if rg -n 'pipeline\s+default\s*\([^)]*\btask\b' docs/src/cookbook.md; then \
		echo "error: cookbook default pipelines cannot require host-only task input; use a self-contained example or argv" >&2; \
		exit 1; \
	fi
	@echo "    Cookbook entrypoints OK."

# Regenerate the diagnostic-code catalog (markdown page + JSON sidecar)
# from the in-binary registry in crates/harn-parser/src/diagnostic_codes.rs.
# Run this whenever you add, rename, retire, or rewire a HARN-<CAT>-<NNN>
# code or its repair template.
sync-diagnostics-catalog:
	@set -e; \
	tmp_dir=$$(mktemp -d "$(CURDIR)/.diagnostics-catalog.XXXXXX"); \
	tmp_md="$$tmp_dir/diagnostics.md"; \
	tmp_json="$$tmp_dir/diagnostics-catalog.json"; \
	trap 'rm -f "$$tmp_md" "$$tmp_json"; rmdir "$$tmp_dir" 2>/dev/null || true' EXIT; \
	$(HARN_BIN_ASSIGN); \
	case "$$harn_bin" in /*) ;; *) harn_bin="$(CURDIR)/$$harn_bin" ;; esac; \
	"$$harn_bin" explain --catalog --format markdown > "$$tmp_md"; \
	"$$harn_bin" explain --catalog --format json > "$$tmp_json"; \
	mv "$$tmp_md" docs/src/diagnostics.md; \
	mv "$$tmp_json" docs/diagnostics-catalog.json

# CI guard: fail if docs/src/diagnostics.md or docs/diagnostics-catalog.json
# drift from the in-binary registry. `make sync-diagnostics-catalog` fixes it.
check-diagnostics-catalog:
	@echo "=== Checking diagnostic-code catalog is up to date ==="
	@set -e; \
	tmp_md=$$(mktemp); \
	tmp_json=$$(mktemp); \
	trap 'rm -f "$$tmp_md" "$$tmp_json"' EXIT; \
	$(HARN_BIN_ASSIGN); \
	case "$$harn_bin" in /*) ;; *) harn_bin="$(CURDIR)/$$harn_bin" ;; esac; \
	"$$harn_bin" explain --catalog --format markdown > "$$tmp_md"; \
	"$$harn_bin" explain --catalog --format json > "$$tmp_json"; \
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
# Diagnostic fences must reproduce their committed compiler/linter projection.
# Blocks tagged ```harn,ignore are skipped.
sync-docs-diagnostics:
	@$(HARN_CMD) run scripts/check_docs_snippets.harn -- --write-diagnostics

check-docs-snippets:
	@echo "=== Checking docs snippets parse under harn parse ==="
	@$(HARN_CMD) run scripts/check_docs_snippets.harn

# CI guard: no documentation prose or ```harn,ignore body may still name a
# runtime API the compiler has removed. The old->new mapping is read out of
# the compiler's own migration table, never from a list in this repository.
# scripts/docs-removed-symbols-allowlist.txt is a shrinking baseline; the
# gate fails when one of its entries goes stale.
#
# This sits in the portable half even though its verdict comes out of the
# binary's linked migration table, which is why check-drift classifies it
# `binary`. The reason is coverage: a docs-only change skips the heavy Rust
# lanes at PR time and can skip them again as a merge-queue docs-only tail,
# so the exact half never sees it — and a docs-only change is exactly how a
# stale API name gets written. The migration table moves far more slowly
# than the pages do. The residual window is a migration that lands between
# releases *and* gets a new allowlist line; that line would read as stale
# here until the next tag, which is visible to the reviewer rather than
# silent, and adding allowlist lines is already a reviewed act.
check-docs-symbols:
	@echo "=== Checking docs for removed runtime API names ==="
	@$(HARN_CMD) run scripts/check_docs_symbols.harn

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

# CI ratchet: generic embedded stdlib behavior must remain host-neutral. Exact
# literals retained for compatibility or public provenance are reviewed in a
# source-line baseline.
check-stdlib-host-neutral:
	@./scripts/check_stdlib_host_neutral.sh

# CI ratchet: public contract files must not name a specific downstream host.
check-public-product-names:
	@./scripts/check_public_product_names.sh

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

# Zero-baseline strict gate for the std/schema facade: both strict-types and
# strict lint must be clean on stdlib_schema.harn and its contracts. The schema
# boundary owns the canonical validator/result/report contracts, so it must not
# carry grandfathered findings. See scripts/check_schema_strict.sh.
check-schema-strict:
	@./scripts/check_schema_strict.sh

# Structural ratchet: optional deps that disable upstream defaults must keep a
# complete feature set (e.g. ort download-binaries + exactly one TLS feature).
# Cheap; runs in the required audit-scripts lane. See #5690.
check-optional-dep-feature-contracts:
	@$(HARN_CMD) run scripts/check_optional_dep_feature_contracts.harn

# Compile off-by-default feature sets that default CI omits. Excludes
# harn-hostlib/computer-local (desktop capture libs). See #5690.
check-all-features:
	@./scripts/check_all_features.sh

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

# Repo-wide 1500-line ceiling for Rust and stdlib Harn. A stable inventory
# identifies legacy debt; its no-growth ceiling comes from the merge base on
# branches and the first parent on integrated commits.
check-source-file-lengths:
	@$(HARN_NO_BUILD_CMD) run scripts/check_source_file_lengths.harn

check-python-boundary:
	@$(HARN_CMD) run scripts/check_python_boundary.harn

check-loud-boundaries:
	@$(HARN_NO_BUILD_CMD) run scripts/check_loud_boundaries.harn

# Structural seam between the product turn-end check and measurement surfaces.
# The product module may not name grading vocabulary; eval/bench modules may
# not reach into turn-end internals.
check-turn-end-boundary:
	@$(HARN_NO_BUILD_CMD) run scripts/check_turn_end_boundary.harn

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

# Regenerate the compiled parser outputs with the tree-sitter CLI version
# pinned by tree-sitter-harn/package-lock.json.
gen-tree-sitter-parser:
	@./scripts/tree_sitter_generated.sh --write

# Regenerate in an isolated temporary grammar checkout and compare the complete
# parser source tree byte-for-byte without mutating the worktree.
check-tree-sitter-parser:
	@echo "=== Checking tree-sitter parser generated artifacts ==="
	@./scripts/tree_sitter_generated.sh --check

# CI guard: fail if the spec grammar's keyword literals drift from the lexer's
# KEYWORDS const (a keyword renamed/removed in the lexer but left stale in the
# `## Grammar` section). Contextual keywords are allowlisted in the script.
check-grammar-keywords:
	@echo "=== Checking spec grammar keyword literals match the lexer ==="
	@$(HARN_CMD) run scripts/check_grammar_keywords.harn

gen-grammar-fitness:
	@$(HARN_CMD) run scripts/sync_grammar_fitness_receipt.harn

# This generated-artifact guard only verifies that the committed receipt still
# matches its inputs. The semantic parser-agreement corpus is a normal
# harn-hostlib test and is already covered by `make test` / the CI behavior
# suite; compiling it again here would make the script-only audit lane rebuild
# a second Cargo graph.
check-grammar-fitness:
	@echo "=== Checking grammar artifact and corpus fitness receipt ==="
	@$(HARN_CMD) run scripts/sync_grammar_fitness_receipt.harn -- --check

# CI guard: fail if the tree-sitter grammar cannot parse the positive Harn
# source sweep (conformance/tests, examples, tests/bridge). This is the same
# sweep the release grammar-audit lane runs; wiring it into the PR-time audit
# battery means a new .harn test that exercises syntax the editor grammar does
# not cover fails the PR that introduces it, not the next release (see #4908 /
# the v0.10.22 dry-run miss). Assumes the compiled grammar library is already
# present (the CI audit lane builds it before the fanout); on a workstation the
# sweep script compiles it on demand from the committed tree-sitter-harn/src.
verify-tree-sitter-parse:
	@echo "=== Verifying tree-sitter parse coverage across the positive .harn sweep ==="
	@$(HARN_CMD) run scripts/verify_tree_sitter_parse.harn -- --strict

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

# The `#[harn_builtin(exposure = "harness...")]` declarations in harn-vm are the
# owner; this projects their names into the dependency-leaf contracts crate so
# `harn check` can reject an unknown capability method without a running VM.
gen-vm-exposures:
	@$(HARN_CMD) run scripts/gen_vm_capability_exposures.harn

check-vm-exposures:
	@echo "=== Checking VM-declared capability method names are up to date ==="
	@$(HARN_CMD) run scripts/gen_vm_capability_exposures.harn -- --check

check-rust-test-lane-policy:
	@echo "=== Checking Rust test lane stack contract ==="
	@$(HARN_CMD) run scripts/check_rust_test_lane_policy.harn

check-cargo-lock-contract:
	@echo "=== Checking CI cargo lock contract ==="
	@$(HARN_CMD) run scripts/check_cargo_lock_contract.harn

check-agent-guidance:
	@echo "=== Checking canonical agent guidance ==="
	@bash scripts/check_agent_guidance.sh

# Fast "before you declare clean" drift preflight. The member set is DERIVED
# from the [preflight.dispatch] table in scripts/generated_artifacts.toml (the
# single source of truth), tier = source: audits that read committed files at
# interpret time, so they are trustworthy the instant you save a file — no
# rebuild needed. `make check-generated-registry` guarantees every check-*
# target is classified there, so this set can never silently omit a guard.
# `make all` remains the full slow gate; this is the seconds-scale local gate.
# For guards whose verdict depends on current binary semantics (generated
# output, parser, checker, or linter), run `check-drift-binary` after a rebuild
# or with a fresh HARN_BIN; stale executable bytes can false-pass those guards.
check-drift:
	@echo "=== Fast drift preflight (source-reading tier) ==="
	@harn_bin="$$(HARN_BIN_NO_BUILD=1 $(HARN_BIN_PRINT_CMD))" || exit 1; \
	members="$$($$harn_bin run scripts/drift_preflight_members.harn -- --tier source)" || exit 1; \
	$(MAKE) --no-print-directory HARN_BIN="$$harn_bin" $$members
	@echo "    Drift preflight (source) OK."

# Binary-semantics drift tier: each member's verdict depends on current
# executable behavior, so a stale binary can false-pass. Run this only with a
# binary built from current source (after `make build` / with a fresh HARN_BIN);
# CI is safe because CI's HARN_BIN is freshly built.
check-drift-binary:
	@echo "=== Drift preflight (binary-semantics tier; needs a fresh HARN_BIN) ==="
	@harn_bin="$$(HARN_BIN_NO_BUILD=1 $(HARN_BIN_PRINT_CMD))" || exit 1; \
	members="$$($$harn_bin run scripts/drift_preflight_members.harn -- --tier binary)" || exit 1; \
	$(MAKE) --no-print-directory HARN_BIN="$$harn_bin" $$members
	@echo "    Drift preflight (binary) OK."
