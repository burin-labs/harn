#!/usr/bin/env bash
# Unit test for scripts/check_release_smoke.sh with a mocked `gh`.
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
script="$repo_root/scripts/check_release_smoke.sh"
tmp_root="$(mktemp -d)"
trap 'rm -rf "$tmp_root"' EXIT

fail() {
  echo "check_release_smoke_test: $*" >&2
  exit 1
}

PATH="$tmp_root/bin:$PATH"
mkdir -p "$tmp_root/bin"

cat >"$tmp_root/bin/gh" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
case "${1:-}" in
  repo)
    echo '{"nameWithOwner":"burin-labs/harn"}'
    ;;
  run)
    if [[ "${2:-}" == "list" ]]; then
      cat <<'JSON'
[
  {
    "databaseId": 111,
    "conclusion": "skipped",
    "event": "workflow_run",
    "displayTitle": "Release smoke (main)",
    "headBranch": "main",
    "url": "https://example.test/111",
    "createdAt": "2026-07-17T23:00:00Z"
  },
  {
    "databaseId": 222,
    "conclusion": "success",
    "event": "workflow_run",
    "displayTitle": "Release smoke (v0.10.23)",
    "headBranch": "main",
    "url": "https://example.test/222",
    "createdAt": "2026-07-17T23:45:00Z"
  },
  {
    "databaseId": 333,
    "conclusion": "success",
    "event": "workflow_run",
    "displayTitle": "Release smoke",
    "headBranch": "main",
    "url": "https://example.test/333",
    "createdAt": "2026-07-20T04:54:00Z"
  }
]
JSON
    elif [[ "${2:-}" == "view" ]]; then
      run_id="${3:-}"
      if [[ "$run_id" == "333" ]]; then
        cat <<'LOG'
Resolve release smoke input	Resolve smoke mode	2026-07-20T04:54:27.3141475Z ##[notice]All release assets for v0.10.29 are available; smoke jobs will download official artifacts.
Release smoke (linux)	UNKNOWN STEP	2026-07-20T04:54:34.2023586Z   ref: v0.10.29
LOG
      else
        echo "no log markers for run $run_id" >&2
        exit 1
      fi
    else
      echo "unexpected gh run invocation: $*" >&2
      exit 99
    fi
    ;;
  *)
    echo "unexpected gh invocation: $*" >&2
    exit 99
    ;;
esac
EOF
chmod +x "$tmp_root/bin/gh"

if "$script" 2>"$tmp_root/usage.err"; then
  fail "expected usage failure without a tag"
fi
grep -q "usage:" "$tmp_root/usage.err" || fail "usage stderr missing"

if ! out="$("$script" v0.10.23)"; then
  fail "expected success for tagged run-name match"
fi
grep -q "covered by run 222" <<<"$out" || fail "success output missing run id: $out"
grep -q "https://example.test/222" <<<"$out" || fail "success output missing url: $out"

if ! out="$("$script" v0.10.29)"; then
  fail "expected success via log-marker fallback for legacy run titles"
fi
grep -q "covered by run 333" <<<"$out" || fail "log fallback missed run 333: $out"

if "$script" v0.10.99 2>"$tmp_root/missing.err"; then
  fail "expected failure when no covering run exists"
fi
grep -q "no successful Release smoke run found for v0.10.99" "$tmp_root/missing.err" \
  || fail "missing-tag stderr incorrect"
grep -q "declarable-ready" "$tmp_root/missing.err" || fail "checklist wording missing"

echo "check_release_smoke_test: ok"
