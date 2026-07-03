#!/usr/bin/env bash
set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
publish_script="$repo_root/scripts/publish.sh"

tmp_root=$(mktemp -d)
trap 'rm -rf "$tmp_root"' EXIT

fake_bin="$tmp_root/bin"
mkdir -p "$fake_bin"

metadata_file="$tmp_root/metadata.json"
record_file="$tmp_root/published-order.txt"
curl_state_dir="$tmp_root/curl-state"
mkdir -p "$curl_state_dir"

cat > "$metadata_file" <<'JSON'
{
  "packages": [
    {
      "name": "gamma",
      "version": "1.2.3",
      "id": "path+file:///repo/gamma#1.2.3",
      "publish": null,
      "dependencies": [
        {"name": "beta", "source": null},
        {"name": "alpha", "source": null}
      ]
    },
    {
      "name": "private-helper",
      "version": "1.2.3",
      "id": "path+file:///repo/private-helper#1.2.3",
      "publish": [],
      "dependencies": []
    },
    {
      "name": "alpha",
      "version": "1.2.3",
      "id": "path+file:///repo/alpha#1.2.3",
      "publish": null,
      "dependencies": []
    },
    {
      "name": "beta",
      "version": "1.2.3",
      "id": "path+file:///repo/beta#1.2.3",
      "publish": null,
      "dependencies": [
        {"name": "alpha", "source": null}
      ]
    }
  ],
  "workspace_members": [
    "path+file:///repo/gamma#1.2.3",
    "path+file:///repo/private-helper#1.2.3",
    "path+file:///repo/alpha#1.2.3",
    "path+file:///repo/beta#1.2.3"
  ]
}
JSON

cat > "$fake_bin/cargo" <<'SH'
#!/usr/bin/env bash
set -euo pipefail

if [[ "${1:-}" == "metadata" ]]; then
  cat "$FAKE_CARGO_METADATA"
  exit 0
fi

if [[ "${1:-}" == "publish" && "${2:-}" == "--workspace" ]]; then
  echo "error: crate alpha@1.2.3 already exists on crates.io index" >&2
  exit 1
fi

if [[ "${1:-}" == "publish" && "${2:-}" == "-p" ]]; then
  crate="${3:-}"
  printf '%s\n' "$crate" >> "$FAKE_CARGO_RECORD"
  if [[ "$crate" == "private-helper" ]]; then
    echo "private-helper should not be published" >&2
    exit 2
  fi
  echo "error: failed to publish $crate v1.2.3 to registry at https://crates.io" >&2
  echo "Caused by:" >&2
  echo "  the remote server responded with an error (status 400 Bad Request): retryable registry response after upload" >&2
  exit 1
fi

echo "unexpected fake cargo invocation: $*" >&2
exit 2
SH
chmod +x "$fake_bin/cargo"

cat > "$fake_bin/curl" <<'SH'
#!/usr/bin/env bash
set -euo pipefail

url=""
for arg in "$@"; do
  case "$arg" in
    https://crates.io/api/v1/crates/*)
      url="$arg"
      ;;
  esac
done

if [[ -z "$url" ]]; then
  echo "fake curl expected crates.io API URL" >&2
  exit 2
fi

crate_version="${url#https://crates.io/api/v1/crates/}"
crate="${crate_version%%/*}"
state="$FAKE_CURL_STATE_DIR/$crate"

if [[ -e "$state" ]]; then
  exit 0
fi

touch "$state"
exit 22
SH
chmod +x "$fake_bin/curl"

cat > "$fake_bin/tee" <<'SH'
#!/usr/bin/env bash
set -euo pipefail

append=false
if [[ "${1:-}" == "-a" ]]; then
  append=true
  shift
fi

target="${1:-}"
if [[ -z "$target" ]]; then
  echo "fake tee expected a target file" >&2
  exit 2
fi

tmp="$(mktemp)"
cat > "$tmp"
/bin/sleep 0.05
if [[ "$append" == "true" ]]; then
  cat "$tmp" >> "$target"
else
  cat "$tmp" > "$target"
fi
cat "$tmp"
rm -f "$tmp"
SH
chmod +x "$fake_bin/tee"

cat > "$fake_bin/sleep" <<'SH'
#!/usr/bin/env bash
exit 0
SH
chmod +x "$fake_bin/sleep"

PATH="$fake_bin:$PATH" \
  FAKE_CARGO_METADATA="$metadata_file" \
  FAKE_CARGO_RECORD="$record_file" \
  FAKE_CURL_STATE_DIR="$curl_state_dir" \
  "$publish_script" > "$tmp_root/output.txt"

expected_order=$'alpha\nbeta\ngamma'
actual_order=$(cat "$record_file")
if [[ "$actual_order" != "$expected_order" ]]; then
  printf 'expected publish order:\n%s\nactual:\n%s\n' "$expected_order" "$actual_order" >&2
  exit 1
fi

if grep -q "private-helper" "$record_file"; then
  echo "publish fallback attempted a publish=false workspace crate" >&2
  exit 1
fi

grep -q "Workspace publish complete" "$tmp_root/output.txt"
grep -q "already published at this version" "$tmp_root/output.txt"

release_root="$tmp_root/release-root"
mkdir -p "$release_root"
cat > "$release_root/Cargo.toml" <<'EOF'
[workspace]
version = "1.2.3"
members = []
EOF

fake_publish="$tmp_root/fake-publish.sh"
cat > "$fake_publish" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
printf 'pwd=%s args=%s\n' "$PWD" "$*" > "$FAKE_RELEASE_GATE_RECORD"
SH
chmod +x "$fake_publish"

release_gate_record="$tmp_root/release-gate-record.txt"
HARN_RELEASE_ROOT="$release_root" \
  HARN_PUBLISH_SCRIPT="$fake_publish" \
  FAKE_RELEASE_GATE_RECORD="$release_gate_record" \
  "$repo_root/scripts/release_gate.sh" publish --dry-run > "$tmp_root/release-gate-output.txt"

expected_gate_record="pwd=$release_root args=--dry-run"
actual_gate_record=$(cat "$release_gate_record")
if [[ "$actual_gate_record" != "$expected_gate_record" ]]; then
  printf 'expected release_gate to run publish script against release root:\n%s\nactual:\n%s\n' \
    "$expected_gate_record" "$actual_gate_record" >&2
  exit 1
fi

echo "publish_script_test: ok"
