#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"

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
import re, sys
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
import re, sys

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
# major/minor bump keeps harn-* path deps resolvable against the new
# version line. Patch bumps within a X.Y line are no-ops here.
crate_dirs = [p for p in Path("crates").iterdir() if p.is_dir()]
harn_crates = {p.name for p in crate_dirs}
pattern = re.compile(
    r'(harn-[A-Za-z0-9_-]+)(\s*=\s*\{\s*path\s*=\s*"[^"]+"\s*,\s*version\s*=\s*)"([^"]+)"'
)


def rewrite(match: re.Match) -> str:
    name = match.group(1)
    if name not in harn_crates:
        return match.group(0)
    return f'{name}{match.group(2)}"{major_minor}"'


for crate_dir in crate_dirs:
    manifest = crate_dir / "Cargo.toml"
    if not manifest.exists():
        continue
    original = manifest.read_text()
    new_text = pattern.sub(rewrite, original)
    if new_text != original:
        manifest.write_text(new_text)
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

run_docs_audit() {
  time_phase "sync_language_spec" ./scripts/sync_language_spec.sh
  time_phase "markdownlint" npx markdownlint-cli2 "**/*.md"
  if command -v mdbook >/dev/null 2>&1; then
    time_phase "mdbook build" mdbook build docs
  else
    echo "warning: mdbook not installed; skipping mdbook build"
  fi
}

run_grammar_audit() {
  if [[ ! -f spec/HARN_SPEC.md ]]; then
    echo "error: missing spec/HARN_SPEC.md"
    return 1
  fi
  time_phase "verify_release_metadata" ./scripts/verify_release_metadata.py
  time_phase "sync_language_spec" ./scripts/sync_language_spec.sh
  time_phase "verify_language_spec" ./scripts/verify_language_spec.py
  if [[ ! -d tree-sitter-harn ]]; then
    echo "warning: tree-sitter-harn not present; skipping tree-sitter grammar audit"
    return 0
  fi
  time_phase "verify_tree_sitter_parse" ./scripts/verify_tree_sitter_parse.py --strict
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
  time_phase "harn lint" make lint-harn
  time_phase "harn fmt --check" make fmt-harn
}

cmd_audit() {
  echo "=== Parallel release audit ==="
  local audit_started
  audit_started="$(date +%s)"

  # Serial warm prebuild before spawning the parallel lanes. The 3
  # cargo-using lanes (rust-audit runs clippy + nextest; harn-audit
  # runs `cargo build --bin harn` via `make lint-harn` and `cargo run`
  # via conformance/fmt-harn; grammar-audit shells `target/debug/harn`
  # via verify_language_spec.py) otherwise race for the same
  # `.cargo-lock` and repeatedly invalidate each other's incremental
  # artifacts. Historically the harn lint phase alone stretched to
  # ~12 min cold because it was waiting on the shared target dir
  # while rust-audit's nextest held the lock; warm-state it runs in
  # ~1.5 s. One serial build up front lets every downstream lane hit
  # a populated target/debug.
  local prebuild_started prebuild_elapsed
  prebuild_started="$(date +%s)"
  echo ">>> warm-prebuild (cargo build --workspace --all-targets)"
  if ! cargo build --workspace --all-targets --quiet; then
    echo "error: warm prebuild failed; rerun without --quiet for details"
    exit 1
  fi
  prebuild_elapsed=$(( $(date +%s) - prebuild_started ))
  printf 'ok: %-15s (%ss)\n' "warm-prebuild" "$prebuild_elapsed"

  local tmp
  tmp="$(mktemp -d)"
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

  run_step rust-audit run_rust_audit & steps+=("rust-audit") pids+=("$!")
  run_step harn-audit run_harn_audit & steps+=("harn-audit") pids+=("$!")
  run_step docs-audit run_docs_audit & steps+=("docs-audit") pids+=("$!")
  run_step grammar-audit run_grammar_audit & steps+=("grammar-audit") pids+=("$!")
  run_step security-audit run_security_audit & steps+=("security-audit") pids+=("$!")
  run_step package-audit ./scripts/verify_crate_packages.sh & steps+=("package-audit") pids+=("$!")

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
    echo ""
    echo "=== Failed audit steps ==="
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
  while [[ $# -gt 0 ]]; do
    case "$1" in
      --bump)
        bump="${2:-}"
        shift 2
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
  require_clean_tree
  local current next
  current="$(current_version)"
  next="$(next_version "$bump")"
  bump_version "$next"
  cargo check --workspace --all-targets >/dev/null
  echo "Version updated: $current -> $next"
  echo "Next steps:"
  echo "  1. Review docs/release notes diff"
  echo "  2. Commit on a release/v$next branch: git commit -am 'Bump version to $next'"
  echo "  3. Open a PR into main and let it land through the merge queue"
  echo "  4. Finalize after merge: ./scripts/release_ship.sh --finalize"
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
  ./scripts/publish.sh ${dry_run}
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
    python3 scripts/render_release_notes.py --version "$version" --output "$output"
    echo "Rendered release notes for ${version#v} -> $output"
  else
    python3 scripts/render_release_notes.py --version "$version"
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
