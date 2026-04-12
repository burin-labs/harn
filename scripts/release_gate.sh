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
path = Path("Cargo.toml")
text = path.read_text()
updated, count = re.subn(r'^version = "[^"]+"', f'version = "{next_version}"', text, count=1, flags=re.M)
if count != 1:
    raise SystemExit("failed to update workspace version")
path.write_text(updated)
PY
}

run_docs_audit() {
  ./scripts/sync_language_spec.sh
  npx markdownlint-cli2 "**/*.md"
  if command -v mdbook >/dev/null 2>&1; then
    mdbook build docs
  else
    echo "warning: mdbook not installed; skipping mdbook build"
  fi
}

run_grammar_audit() {
  if [[ ! -f spec/HARN_SPEC.md ]]; then
    echo "error: missing spec/HARN_SPEC.md"
    return 1
  fi
  ./scripts/verify_release_metadata.py
  ./scripts/sync_language_spec.sh
  ./scripts/verify_language_spec.py
  if [[ ! -d tree-sitter-harn ]]; then
    echo "warning: tree-sitter-harn not present; skipping tree-sitter grammar audit"
    return 0
  fi
  ./scripts/verify_tree_sitter_parse.py --strict
  (
    cd tree-sitter-harn
    npm test
  )
}

run_security_audit() {
  echo "=== Security/trust boundary audit ==="
  rg -n "OAuth|oauth|MCP|trust boundary|mutation session|worker_update|tool/pre_use|tool/post_use" \
    README.md docs/src crates/harn-vm crates/harn-cli .github CLAUDE.md >/dev/null
}

run_rust_audit() {
  make fmt-check
  cargo clippy --workspace --all-targets -- -D warnings
  make test
}

run_harn_audit() {
  make conformance
  make lint-harn
  make fmt-harn
}

cmd_audit() {
  echo "=== Parallel release audit ==="
  local tmp
  tmp="$(mktemp -d)"
  local -a steps=()
  local -a pids=()

  run_step() {
    local name="$1"
    shift
    (
      set -euo pipefail
      echo ">>> $name"
      "$@"
    ) >"$tmp/$name.log" 2>&1
  }

  run_step rust-audit run_rust_audit & steps+=("rust-audit") pids+=("$!")
  run_step harn-audit run_harn_audit & steps+=("harn-audit") pids+=("$!")
  run_step docs-audit run_docs_audit & steps+=("docs-audit") pids+=("$!")
  run_step grammar-audit run_grammar_audit & steps+=("grammar-audit") pids+=("$!")
  run_step security-audit run_security_audit & steps+=("security-audit") pids+=("$!")

  local failed=0
  local idx
  for idx in "${!steps[@]}"; do
    local step="${steps[$idx]}"
    local pid="${pids[$idx]}"
    if wait "$pid"; then
      echo "ok: $step"
    else
      echo "fail: $step"
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
  echo "=== Audit complete ==="
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
  cargo check --workspace >/dev/null
  echo "Version updated: $current -> $next"
  echo "Next steps:"
  echo "  1. Review docs/release notes diff"
  echo "  2. Commit: git commit -am 'Bump version to $next'"
  echo "  3. Tag after merge or final verification: git tag v$next"
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
  echo "Publish phase complete for v$version"
  echo "Follow-up:"
  echo "  git tag v$version"
  echo "  git push origin v$version"
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
