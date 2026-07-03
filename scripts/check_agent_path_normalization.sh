#!/usr/bin/env bash
#
# Regression guard: agent-facing paths must be forward-slash-normalized.
#
# harn-hostlib is a coding-agent tool surface. Every filesystem path it hands
# back to the model / a pipeline / a VmValue MUST use `/` separators on every
# platform. `Path::display()` and `to_string_lossy()` emit OS-native
# separators — backslashes on Windows — so a raw path string leaks
# `crates\foo\bar.rs` into tool output. That broke the Windows-only CI test
# `tools_search::search_glob_filter_does_not_reinclude_gitignored_paths`
# (#3914 shipped `path.to_string_lossy()` with no cross-platform coverage).
#
# The fix routed every agent-facing emission through the single chokepoint
# `crate::tools::args::to_agent_path{,_str}`. This gate keeps it that way.
#
# What it flags: a path rendering (`to_string_lossy()` or `.display()`) that
# is passed, ON THE SAME LINE, straight into an agent-facing string sink —
# `str_value(...)`, a `.str("...", ...)` response-builder call, or a
# `serde_json::Value::String(...)` — without going through `to_agent_path`.
# That is the exact shape of the original bug.
#
# Escape hatch: a genuinely non-agent-facing line (an on-disk journal/manifest
# serialization, a debug label) may opt out with a trailing
# `// agent-path-ok: <reason>` comment. Every opt-out is therefore visible in
# review, and the reason documents WHY the separator does not reach the model.
#
# Why not also ban the raw `.replace('\\', "/")` idiom? Because that spelling
# is legitimately used for glob-pattern input normalization and for path
# EQUALITY comparisons (both sides normalized), which are not emissions. The
# sink-shaped check above is precise: it fires only when an OS-native path
# rendering flows directly into agent output.
#
# Run with `--self-test` to verify the detector still catches a planted
# regression and still passes clean code.

set -euo pipefail

repo_root="$(cd "$(dirname "$0")/.." && pwd)"

# Line-local match: an agent-facing string sink whose argument is rendered
# with a raw OS-native path method, on the same line.
sink_re='(str_value\(|\.str\(|serde_json::Value::String\().*(to_string_lossy\(\)|\.display\(\))'

fail=0

find_hits() {
  local root="$1"
  # Candidate lines: sink shape present, not already normalized, not opted out.
  grep -rns --include='*.rs' -E "$sink_re" "$root" 2>/dev/null \
    | grep -v 'to_agent_path' \
    | grep -v 'agent-path-ok:' || true
}

scan() {
  local root="$1"
  local hits
  hits="$(find_hits "$root")"
  if [ -n "$hits" ]; then
    echo "::error::Agent-facing string emits a raw OS-native path (to_string_lossy/display)."
    echo "  On Windows this ships backslashes to the model. Wrap the path in"
    echo "  crate::tools::args::to_agent_path(&path). If the string genuinely never"
    echo "  reaches the agent (on-disk journal/manifest, debug label), append a"
    echo "  trailing '// agent-path-ok: <reason>' comment to the line."
    echo "$hits" | sed 's/^/    /'
    fail=1
  fi
}

self_test() {
  local tmp
  tmp="$(mktemp -d)"
  trap 'rm -rf "$tmp"' RETURN

  # Planted regressions of the banned shape (one per sink flavor).
  cat > "$tmp/bad.rs" <<'EOF'
fn a(p: &std::path::Path) -> VmValue { str_value(p.to_string_lossy()) }
fn b(p: &std::path::Path) { builder.str("output_path", p.display().to_string()); }
fn c(p: &std::path::Path) { serde_json::Value::String(p.display().to_string()); }
EOF
  if [ -z "$(find_hits "$tmp")" ]; then
    echo "self-test FAILED: detector did not flag planted regressions" >&2
    exit 1
  fi

  # Clean code that must NOT trip the gate:
  #  - helper use
  #  - internal display() in an error message / syscall arg
  #  - a legitimately-opted-out on-disk serialization line
  #  - a glob-input / equality .replace('\\', "/") (must stay allowed)
  cat > "$tmp/good.rs" <<'EOF'
fn a(p: &std::path::Path) -> VmValue { str_value(to_agent_path(p)) }
fn b(p: &std::path::Path) { let _ = format!("read `{}`: err", p.display()); }
fn c(p: &std::path::Path) { std::fs::File::open(p).ok(); }
fn d(p: &std::path::Path) { serde_json::Value::String(p.to_string_lossy().into_owned()); } // agent-path-ok: on-disk journal line
fn e(g: &str) -> String { g.replace('\\', "/") }
fn f(a: &str, b: &str) -> bool { a.replace('\\', "/") == b.replace('\\', "/") }
EOF
  rm "$tmp/bad.rs"
  local clean_hits
  clean_hits="$(find_hits "$tmp")"
  if [ -n "$clean_hits" ]; then
    echo "self-test FAILED: detector flagged clean code" >&2
    echo "$clean_hits" | sed 's/^/    /' >&2
    exit 1
  fi

  echo "check_agent_path_normalization self-test passed."
}

if [ "${1:-}" = "--self-test" ]; then
  self_test
  exit 0
fi

scan "${1:-$repo_root/crates/harn-hostlib/src}"

if [ "$fail" -ne 0 ]; then
  echo "::error::Agent-facing path normalization guard failed. See crate::tools::args::to_agent_path." >&2
  exit 1
fi

echo "Agent-facing path normalization guard passed."
