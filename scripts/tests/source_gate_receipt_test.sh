#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd -P)"
# shellcheck source=scripts/lib/source_gate_receipt.sh
source "$repo_root/scripts/lib/source_gate_receipt.sh"

tmp_root="$(mktemp -d)"
trap 'rm -rf "$tmp_root"' EXIT
fixture="$tmp_root/repo"
mkdir -p "$fixture/bin"
git -C "$fixture" init -q
git -C "$fixture" config user.name "Harn test"
git -C "$fixture" config user.email "harn-test@example.invalid"
git -C "$fixture" config commit.gpgSign false
printf 'one\n' > "$fixture/source.txt"
printf '.harn/\n' > "$fixture/.gitignore"
git -C "$fixture" add source.txt .gitignore
git -C "$fixture" commit -qm initial

fake_harn="$fixture/bin/harn"
cat > "$fake_harn" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
case "${1:-} ${2:-}" in
  "__internal-source-gate-receipt-v1 write")
    mkdir -p "$(dirname "$3")"
    printf 'head=%s\nbinary=%s\nbuild=%s\n' "$4" "$6" "$7" > "$3"
    ;;
  "__internal-source-gate-receipt-v1 verify")
    grep -Fxq "head=$4" "$3"
    grep -Fxq "binary=$5" "$3"
    grep -Fxq "build=$6" "$3"
    ;;
  *) exit 2 ;;
esac
SH
chmod +x "$fake_harn"
git -C "$fixture" add bin/harn
git -C "$fixture" commit -qm binary

export GITHUB_ACTIONS=true
export SOURCE_GATE_CI_BINARY_COMMIT="$(git -C "$fixture" rev-parse HEAD)"
export SOURCE_GATE_CI_BINARY_BUILD_FRESHNESS_ID="$(printf 'a%.0s' {1..40})"
export SOURCE_GATE_CI_BINARY_SHA256="$(harn_source_gate_sha256 "$fake_harn")"

(
  cd "$fixture"
  receipt="$fixture/.harn/receipts/gate.json"
  harn_source_gate_begin "$receipt"
  harn_source_gate_bind_binary "$fake_harn"
  harn_source_gate_finish "$receipt" full_io worker 2 3 passed make conformance
  harn_source_gate_verify "$receipt" "$fake_harn"

  printf 'edited\n' >> source.txt
  if harn_source_gate_verify "$receipt" "$fake_harn" >"$tmp_root/stale-edit.out" 2>&1; then
    echo "source gate accepted a receipt after a source edit" >&2
    exit 1
  fi
  git add source.txt
  git commit -qm amended-source
  export SOURCE_GATE_CI_BINARY_COMMIT="$(git rev-parse HEAD)"
  if harn_source_gate_verify "$receipt" "$fake_harn" >"$tmp_root/stale-head.out" 2>&1; then
    echo "source gate accepted a receipt after the tested commit changed" >&2
    exit 1
  fi

  unset GITHUB_ACTIONS SOURCE_GATE_CI_BINARY_COMMIT SOURCE_GATE_CI_BINARY_BUILD_FRESHNESS_ID SOURCE_GATE_CI_BINARY_SHA256
  if harn_source_gate_binary_identity "$fake_harn" "$(git rev-parse HEAD)" \
      >"$tmp_root/unproved-bin.out" 2>&1; then
    echo "source gate accepted an explicit executable without provenance" >&2
    exit 1
  fi
)

echo "source_gate_receipt_test: ok"
