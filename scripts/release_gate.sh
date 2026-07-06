#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="${HARN_RELEASE_ROOT:-$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)}"
cd "$ROOT_DIR"
PUBLISH_SCRIPT="${HARN_PUBLISH_SCRIPT:-./scripts/publish.sh}"
if [[ -f "$ROOT_DIR/scripts/lib/cargo_env.sh" ]]; then
  # shellcheck source=scripts/lib/cargo_env.sh
  source "$ROOT_DIR/scripts/lib/cargo_env.sh"
fi

release_gate_target_name() {
  printf '%s' "$(basename "$ROOT_DIR")" | tr -c 'A-Za-z0-9._-' '-'
}

default_release_gate_target_dir() {
  local tmp_root
  tmp_root="${TMPDIR:-/tmp}"
  tmp_root="${tmp_root%/}"
  printf '%s/harn-release-gate-target-%s\n' "$tmp_root" "$(release_gate_target_name)"
}

configure_release_gate_cargo_env() {
  if [[ -z "${CARGO_TARGET_DIR:-}" ]]; then
    export CARGO_TARGET_DIR
    CARGO_TARGET_DIR="$(default_release_gate_target_dir)"
  fi
  if [[ -z "${CARGO_BUILD_BUILD_DIR:-}" ]]; then
    if declare -F harn_export_cargo_build_dir_under_target >/dev/null; then
      harn_export_cargo_build_dir_under_target "$CARGO_TARGET_DIR" || true
    else
      export CARGO_BUILD_BUILD_DIR
      CARGO_BUILD_BUILD_DIR="$CARGO_TARGET_DIR/build"
    fi
  fi
}

usage() {
  cat <<'EOF'
Usage:
  ./scripts/release_gate.sh audit
  ./scripts/release_gate.sh prepare --bump patch|minor|major
  ./scripts/release_gate.sh publish [--dry-run]
  ./scripts/release_gate.sh notes [--version vX.Y.Z] [--output file]
  ./scripts/release_gate.sh full --bump patch|minor|major [--dry-run]

Commands:
  audit    Run the release-quality verification gate and docs audit.
  prepare  Bump the workspace version locally and print next tag/release steps.
  publish  Publish crates with scripts/publish.sh and print tag/release follow-up.
  notes    Render GitHub release notes for a version from CHANGELOG.md.
  full     Run audit, prepare, and publish in sequence.
EOF
}

configure_release_gate_cargo_env

require_clean_tree() {
  if ! git diff --quiet --ignore-submodules HEAD --; then
    echo "error: working tree is dirty"
    echo "hint: commit or stash changes before prepare/publish"
    exit 1
  fi
}

current_version() {
  python3 - <<'PY'
from pathlib import Path
import re
text = Path("Cargo.toml").read_text()
m = re.search(r'^version = "([^"]+)"', text, re.M)
print(m.group(1) if m else "")
PY
}

next_version() {
  local bump="$1"
  python3 - "$bump" <<'PY'
from pathlib import Path
import json, re, sys
bump = sys.argv[1]
text = Path("Cargo.toml").read_text()
m = re.search(r'^version = "([^"]+)"', text, re.M)
if not m:
    raise SystemExit("missing workspace version")
major, minor, patch = map(int, m.group(1).split("."))
if bump == "major":
    major, minor, patch = major + 1, 0, 0
elif bump == "minor":
    minor, patch = minor + 1, 0
elif bump == "patch":
    patch += 1
else:
    raise SystemExit(f"unsupported bump: {bump}")
print(f"{major}.{minor}.{patch}")
PY
}

bump_version() {
  local next="$1"
  python3 - "$next" <<'PY'
from pathlib import Path
import json, re, sys, tomllib

next_version = sys.argv[1]
major_minor = ".".join(next_version.split(".")[:2])

root = Path("Cargo.toml")
text = root.read_text()
updated, count = re.subn(
    r'^version = "[^"]+"', f'version = "{next_version}"', text, count=1, flags=re.M
)
if count != 1:
    raise SystemExit("failed to update workspace version")
root.write_text(updated)

# Update inter-crate dep specs across workspace + excluded crates so a
# major/minor bump keeps local path deps resolvable against the new
# version line. Patch bumps within a X.Y line are no-ops here.
workspace = tomllib.loads(root.read_text()).get("workspace", {})


def workspace_package_manifests() -> list[Path]:
    manifests: set[Path] = set()
    for key in ("members", "exclude"):
        for entry in workspace.get(key, []):
            paths = list(Path().glob(entry)) if any(ch in entry for ch in "*?[") else [Path(entry)]
            for path in paths:
                manifest = path if path.name == "Cargo.toml" else path / "Cargo.toml"
                if manifest.exists():
                    manifests.add(manifest)
    return sorted(manifests)


package_manifests = workspace_package_manifests()
local_packages: set[str] = set()
for manifest in package_manifests:
    data = tomllib.loads(manifest.read_text())
    name = data.get("package", {}).get("name")
    if isinstance(name, str) and name:
        local_packages.add(name)

pattern = re.compile(
    r'([A-Za-z0-9_-]+)(\s*=\s*\{\s*path\s*=\s*"[^"]+"\s*,\s*version\s*=\s*)"([^"]+)"'
)


def rewrite(match: re.Match) -> str:
    name = match.group(1)
    if name not in local_packages:
        return match.group(0)
    return f'{name}{match.group(2)}"{major_minor}"'


for manifest in [root, *package_manifests]:
    original = manifest.read_text()
    new_text = pattern.sub(rewrite, original)
    if new_text != original:
        manifest.write_text(new_text)

# Keep the checked-in ACP Agent Registry submission aligned with the
# release being prepared. The registry requires concrete archive URLs
# for the first submission; after listing, its own updater follows
# GitHub Releases, but this local artifact should not drift again.
agent_manifest = Path("spec/acp-registry/harn/agent.json")
if agent_manifest.exists():
    data = json.loads(agent_manifest.read_text())
    data["version"] = next_version
    binary = data.get("distribution", {}).get("binary", {})
    if isinstance(binary, dict):
        for entry in binary.values():
            if not isinstance(entry, dict):
                continue
            archive = entry.get("archive")
            if not isinstance(archive, str) or "/" not in archive:
                continue
            filename = archive.rsplit("/", 1)[-1]
            entry["archive"] = (
                f"https://github.com/burin-labs/harn/releases/download/"
                f"v{next_version}/{filename}"
            )
    agent_manifest.write_text(json.dumps(data, indent=2) + "\n")
PY
}

# Wrap a command with a banner + duration. Used by the per-audit
# substep helpers so the parallel audit log shows which sub-phase in
# `rust-audit` / `harn-audit` / etc is the long pole.
time_phase() {
  local label="$1"
  shift
  local started
  started="$(date +%s)"
  printf '  -> %s ...\n' "$label"
  "$@"
  local rc=$?
  printf '  <- %s (%ss)\n' "$label" "$(( $(date +%s) - started ))"
  return "$rc"
}

debug_harn_binary() {
  local target_dir="${CARGO_TARGET_DIR:-}"
  if [[ -z "$target_dir" ]]; then
    target_dir="$(cargo metadata --format-version=1 --no-deps \
      | python3 -c 'import json, sys; print(json.load(sys.stdin)["target_directory"])')"
  fi
  local suffix=""
  case "${OS:-$(uname -s)}" in
    Windows_NT|MINGW*|MSYS*|CYGWIN*) suffix=".exe" ;;
  esac
  printf '%s/debug/harn%s\n' "$target_dir" "$suffix"
}

harn_cmd() {
  if [[ -n "${HARN_BIN:-}" ]]; then
    "$HARN_BIN" "$@"
  else
    cargo run --quiet --bin harn -- "$@"
  fi
}

run_docs_audit() {
  time_phase "sync_language_spec" harn_cmd run scripts/sync_language_spec.harn
  time_phase "markdownlint" npx markdownlint-cli2 "**/*.md"
  if command -v npm >/dev/null 2>&1; then
    time_phase "docs site build" ./scripts/build_docs_site.sh
  else
    echo "warning: npm (Node.js) not installed; skipping docs site build"
  fi
  time_phase "docs model refs" harn_cmd run scripts/check_docs_model_refs.harn
  time_phase "docs snippets" harn_cmd run scripts/check_docs_snippets.harn
}

run_grammar_audit() {
  if [[ ! -f spec/HARN_SPEC.md ]]; then
    echo "error: missing spec/HARN_SPEC.md"
    return 1
  fi
  time_phase "verify_release_metadata" harn_cmd run scripts/verify_release_metadata.harn
  # NOTE: `sync_language_spec` is intentionally NOT run here. It is the docs
  # mirror writer (spec/HARN_SPEC.md -> docs/src/language-spec.md) and already
  # runs in `run_docs_audit`, which executes in a sibling parallel lane. Running
  # it in both lanes both duplicated ~72s of work and raced two writers on the
  # same `docs/src/language-spec.md` output. `verify_language_spec` below reads
  # the canonical spec source directly (SPEC_PATH = spec/HARN_SPEC.md), not the
  # mirror, so it does not depend on the sync having run in this lane.
  time_phase "verify_language_spec" harn_cmd run scripts/verify_language_spec.harn
  if [[ ! -d tree-sitter-harn ]]; then
    echo "warning: tree-sitter-harn not present; skipping tree-sitter grammar audit"
    return 0
  fi
  time_phase "tree-sitter npm ci" bash -c "cd tree-sitter-harn && npm ci"
  time_phase "verify_tree_sitter_parse" harn_cmd run scripts/verify_tree_sitter_parse.harn -- --strict
  time_phase "tree-sitter npm test" bash -c "cd tree-sitter-harn && npm test"
}

run_security_audit() {
  echo "=== Security/trust boundary audit ==="
  time_phase "boundary-keyword grep" \
    rg -n "OAuth|oauth|MCP|trust boundary|mutation session|worker_update|tool/pre_use|tool/post_use" \
      README.md docs/src crates/harn-vm crates/harn-cli .github CLAUDE.md >/dev/null
}

run_rust_audit() {
  time_phase "cargo fmt --check" make fmt-check
  time_phase "cargo clippy --workspace --all-targets" \
    cargo clippy --workspace --all-targets -- -D warnings
  time_phase "make test (nextest/cargo test)" make test
}

run_harn_audit() {
  time_phase "harn conformance" make conformance
  time_phase "protocol conformance" make protocol-conformance
  time_phase "harn lint" make lint-harn
  time_phase "harn fmt --check" make fmt-harn
}

# Host-platform reproduction of the cross-platform release smoke
# matrix. CI exercises the full macOS/Linux/Windows fan-out via
# .github/workflows/release-smoke.yml; this lane catches host-side
# regressions (binary failing to start, generated artifact emitter
# drift, mock provider plumbing) before a maintainer pushes a release
# tag. Uses the debug binary populated by the warm prebuild so the
# lane does not fight the cargo lock with rust-audit's clippy +
# nextest. The cross-platform deltas still depend on the CI matrix.
run_smoke_audit() {
  time_phase "release smoke" make smoke-audit
}

cmd_audit() {
  echo "=== Parallel release audit ==="
  export CARGO_INCREMENTAL="${CARGO_INCREMENTAL:-0}"
  local audit_started
  audit_started="$(date +%s)"

  # Serial warm prebuild before spawning the parallel lanes. The Harn/script
  # lanes only need a runnable CLI; rust-audit still owns the full clippy +
  # nextest coverage, and package-audit still owns extracted crate checks. Build
  # the CLI binary once up front so those lanes do not race rust-audit for a
  # plain Harn invocation, without front-loading a duplicate workspace build.
  local prebuild_started prebuild_elapsed
  prebuild_started="$(date +%s)"
  echo ">>> warm-prebuild (cargo build -p harn-cli --bin harn)"
  if ! cargo build -p harn-cli --bin harn --quiet; then
    echo "error: warm prebuild failed; rerun without --quiet for details"
    exit 1
  fi
  prebuild_elapsed=$(( $(date +%s) - prebuild_started ))
  printf 'ok: %-15s (%ss)\n' "warm-prebuild" "$prebuild_elapsed"
  if [[ -z "${HARN_BIN:-}" ]]; then
    HARN_BIN="$(debug_harn_binary)"
  fi
  if [[ ! -x "$HARN_BIN" ]]; then
    echo "error: warm prebuild completed but HARN_BIN is not executable: $HARN_BIN"
    exit 1
  fi
  export HARN_BIN
  printf 'ok: %-15s (%s)\n' "harn-bin" "$HARN_BIN"

  local tmp
  tmp="$(mktemp -d)"
  echo "audit lane log dir: $tmp"
  local -a steps=()
  local -a pids=()

  # Each step writes its wall-clock duration to `<name>.dur` so the
  # parent can report per-step timings once everyone wraps. That lets
  # the release gate call out which audit lane is the long pole.
  # With the warm prebuild above, lanes should complete in parallel
  # without fighting for the cargo lock; any lane blowing past ~5 min
  # is a real regression worth investigating.
  run_step() {
    local name="$1"
    shift
    local started
    started="$(date +%s)"
    (
      set -euo pipefail
      echo ">>> $name"
      "$@"
    ) >"$tmp/$name.log" 2>&1
    local rc=$?
    printf '%s\n' "$(( $(date +%s) - started ))" >"$tmp/$name.dur"
    return "$rc"
  }

  launch_step() {
    local name="$1"
    shift
    printf 'log: %-15s (%s)\n' "$name" "$tmp/$name.log"
    run_step "$name" "$@" &
    steps+=("$name")
    pids+=("$!")
  }

  launch_step rust-audit run_rust_audit
  launch_step harn-audit run_harn_audit
  launch_step docs-audit run_docs_audit
  launch_step grammar-audit run_grammar_audit
  launch_step security-audit run_security_audit
  launch_step package-audit ./scripts/verify_crate_packages.sh
  launch_step smoke-audit run_smoke_audit

  local failed=0
  local idx
  for idx in "${!steps[@]}"; do
    local step="${steps[$idx]}"
    local pid="${pids[$idx]}"
    local dur=""
    if wait "$pid"; then
      dur="$([[ -f "$tmp/$step.dur" ]] && cat "$tmp/$step.dur" || echo '?')"
      printf 'ok: %-15s (%ss)\n' "$step" "$dur"
    else
      dur="$([[ -f "$tmp/$step.dur" ]] && cat "$tmp/$step.dur" || echo '?')"
      printf 'fail: %-13s (%ss)\n' "$step" "$dur"
      failed=1
    fi
  done

  if [[ "$failed" -ne 0 ]]; then
    # ── Failure summary FIRST (so the real cause is at the TOP of the
    # output / audit md, not buried thousands of lines into the full dump). ──
    echo ""
    echo "=== RELEASE AUDIT FAILED — failing step(s) ==="
    for step in "${steps[@]}"; do
      local log="$tmp/$step.log"
      [[ -f "$log" ]] || continue
      # Heuristic: a lane failed if it has a `time_phase` sub-step that opened
      # (`  -> label ...`) without a matching close (`  <- label (Ns)`). The
      # last such unmatched label is the failing sub-step. This pinpoints e.g.
      # "grammar-audit / verify_tree_sitter_parse" instead of just "grammar-audit".
      local failing_sub
      failing_sub="$(awk '
        /^  -> / { sub(/^  -> /, ""); sub(/ \.\.\.$/, ""); open=$0 }
        /^  <- / { open="" }
        END { if (open != "") print open }
      ' "$log")"
      # Surface the lane only if it looks like it failed: either it has an
      # unmatched sub-step, or its log contains an obvious error marker near
      # the end. We always include lanes with an unmatched sub-step; for the
      # rest we check the tail for error signatures.
      if [[ -n "$failing_sub" ]] || tail -n 50 "$log" | grep -qiE "error|fail|panic|✗|status completed|sweep failed|assertion"; then
        echo ""
        if [[ -n "$failing_sub" ]]; then
          echo ">>> ${step} / ${failing_sub}  <<< (failing sub-step)"
        else
          echo ">>> ${step}  <<<"
        fi
        echo "    last 40 lines of $step.log:"
        tail -n 40 "$log" | sed 's/^/      /'
      fi
    done

    # ── Full logs AFTER the summary, for deep debugging. ──
    echo ""
    echo "=== Full failed-audit logs ==="
    for step in "${steps[@]}"; do
      if [[ -f "$tmp/$step.log" ]] && [[ -s "$tmp/$step.log" ]]; then
        echo "--- $step ---"
        cat "$tmp/$step.log"
        echo ""
      fi
    done
    rm -rf "$tmp"
    exit 1
  fi

  rm -rf "$tmp"
  local audit_elapsed=$(( $(date +%s) - audit_started ))
  echo "=== Audit complete (${audit_elapsed}s) ==="
}

cmd_prepare() {
  local bump=""
  local allow_dirty=0
  while [[ $# -gt 0 ]]; do
    case "$1" in
      --bump)
        bump="${2:-}"
        shift 2
        ;;
      --allow-dirty)
        # Used by `release_ship.sh --prepare`, which runs from a release
        # branch that already has the human's authored release content
        # (changelog, code, docs) staged or unstaged. The version-bump
        # write below is additive; we don't need a clean tree.
        allow_dirty=1
        shift
        ;;
      *)
        echo "error: unknown prepare arg: $1"
        usage
        exit 1
        ;;
    esac
  done
  if [[ -z "$bump" ]]; then
    echo "error: prepare requires --bump patch|minor|major"
    exit 1
  fi
  if [[ "$allow_dirty" -eq 0 ]]; then
    require_clean_tree
  fi
  local current next
  current="$(current_version)"
  next="$(next_version "$bump")"
  bump_version "$next"
  # The audit lanes that ran before prepare populate cache state keyed to the
  # old workspace crate hashes. Once `bump_version` rewrites Cargo.toml, the
  # prepare-time cargo calls are one-shot rebuilds of generated-artifact
  # helpers; caching them has little value and has failed under macOS sccache
  # file-descriptor pressure. Use direct rustc and skip incremental state for
  # this post-bump phase. `CARGO_BUILD_RUSTC_WRAPPER=` is required because
  # Harn's checked-in `.cargo/config.toml` otherwise still applies sccache.
  export CARGO_INCREMENTAL=0
  export RUSTC_WRAPPER=
  export CARGO_BUILD_RUSTC_WRAPPER=
  export SCCACHE_DISABLE=1

  harn_cmd run scripts/sync_protocol_fixture_runtime_versions.harn -- --from "$current" --to "$next"
  # Keep the embedding guide's `tag = "vX.Y.Z"` pins on the released version
  # line. Nothing owned these before and they silently drifted ~46 versions
  # behind; match any prior version rather than `$current` so a stale doc
  # still converges.
  python3 - "$next" <<'PY'
import re
import sys
from pathlib import Path

next_version = sys.argv[1]
doc = Path("docs/src/embedding-rust.md")
text = doc.read_text()
updated = re.sub(r'tag = "v\d+\.\d+\.\d+"', f'tag = "v{next_version}"', text)
if updated != text:
    doc.write_text(updated)
PY
  # HARN_BIN may still point at the pre-bump binary warmed during the audit
  # phase. Protocol artifacts stamp the compiled crate version, so force this
  # target through a fresh post-bump cargo-built binary. The full workspace was
  # already validated by the release audit before this version-only rewrite, and
  # CI validates the pushed release branch, so do not pay for a second
  # post-bump workspace check here.
  HARN_BIN="" make gen-protocol-artifacts
  echo "Version updated: $current -> $next"
  echo "Next steps:"
  echo "  1. Review docs/release notes diff"
  echo "  2. Commit on a release/v$next branch: git commit -am 'Release v$next'"
  echo "  3. Open a PR into main and let it land through the merge queue"
  echo "  4. Walk away — the publish-release workflow auto-fires on tag drift"
}

cmd_publish() {
  local dry_run=""
  while [[ $# -gt 0 ]]; do
    case "$1" in
      --dry-run)
        dry_run="--dry-run"
        shift
        ;;
      *)
        echo "error: unknown publish arg: $1"
        usage
        exit 1
        ;;
    esac
  done
  if [[ -z "$dry_run" ]]; then
    require_clean_tree
  fi
  "$PUBLISH_SCRIPT" ${dry_run}
  local version
  version="$(current_version)"
  if [[ -n "$dry_run" ]]; then
    echo "Publish dry run complete for v$version"
    return
  fi
  echo "Publish phase complete for v$version"
  echo "Follow-up:"
  echo "  Ensure tag v$version has been pushed from the merge-queue-approved main commit"
  echo "  Review changelog-backed GitHub release notes"
}

cmd_notes() {
  local version=""
  local output=""
  while [[ $# -gt 0 ]]; do
    case "$1" in
      --version)
        version="${2:-}"
        shift 2
        ;;
      --output)
        output="${2:-}"
        shift 2
        ;;
      *)
        echo "error: unknown notes arg: $1"
        usage
        exit 1
        ;;
    esac
  done
  if [[ -z "$version" ]]; then
    version="$(current_version)"
  fi
  if [[ -n "$output" ]]; then
    harn_cmd run scripts/render_release_notes.harn -- --version "$version" --output "$output"
    echo "Rendered release notes for ${version#v} -> $output"
  else
    harn_cmd run scripts/render_release_notes.harn -- --version "$version"
  fi
}

cmd_full() {
  local dry_run=""
  local bump=""
  while [[ $# -gt 0 ]]; do
    case "$1" in
      --dry-run)
        dry_run="--dry-run"
        shift
        ;;
      --bump)
        bump="${2:-}"
        shift 2
        ;;
      *)
        echo "error: unknown full arg: $1"
        usage
        exit 1
        ;;
    esac
  done
  cmd_audit
  cmd_prepare --bump "${bump:-patch}"
  cmd_publish ${dry_run}
}

case "${1:-}" in
  audit)
    shift
    cmd_audit "$@"
    ;;
  prepare)
    shift
    cmd_prepare "$@"
    ;;
  publish)
    shift
    cmd_publish "$@"
    ;;
  notes)
    shift
    cmd_notes "$@"
    ;;
  full)
    shift
    cmd_full "$@"
    ;;
  *)
    usage
    exit 1
    ;;
esac
