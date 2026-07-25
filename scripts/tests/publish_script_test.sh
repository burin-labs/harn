#!/usr/bin/env bash
set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
publish_script="$repo_root/scripts/publish.sh"

tmp_root=$(mktemp -d)
trap 'rm -rf "$tmp_root"' EXIT

fake_harn="$tmp_root/harn"
cat > "$fake_harn" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
printf 'pwd=%s\n' "$PWD" > "$FAKE_HARN_RECORD"
printf 'arg=%s\n' "$@" >> "$FAKE_HARN_RECORD"
SH
chmod +x "$fake_harn"

fake_release_metadata="$tmp_root/release-metadata-harn"
cat > "$fake_release_metadata" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
echo "0.10.39"
SH
chmod +x "$fake_release_metadata"

entry_record="$tmp_root/entry-record.txt"
(
  cd "$repo_root"
  HARN_BIN="$fake_harn" \
    HARN_BIN_NO_BUILD=1 \
    FAKE_HARN_RECORD="$entry_record" \
    "$publish_script" --dry-run
)

expected_entry=$(cat <<EOF
pwd=$repo_root
arg=run
arg=--no-sandbox
arg=$repo_root/scripts/publish.harn
arg=--
arg=--dry-run
EOF
)
if [[ "$(cat "$entry_record")" != "$expected_entry" ]]; then
  printf 'unexpected publish entrypoint invocation:\n%s\n' "$(cat "$entry_record")" >&2
  exit 1
fi

release_root="$tmp_root/release-root"
mkdir -p "$release_root/scripts/lib"
printf '[workspace]\nresolver = "2"\nmembers = []\n' > "$release_root/Cargo.toml"
cp "$repo_root/scripts/harn_bin.sh" "$release_root/scripts/harn_bin.sh"
cp -R "$repo_root/scripts/lib/." "$release_root/scripts/lib/"

release_tools="$tmp_root/release-tools"
mkdir -p "$release_tools/lib"
release_tools=$(cd "$release_tools" && pwd -P)
cp "$publish_script" "$release_tools/publish.sh"
cp "$repo_root/scripts/publish.harn" "$release_tools/publish.harn"
cp "$repo_root/scripts/publish_plan.harn" "$release_tools/publish_plan.harn"
cp "$repo_root/scripts/harn_bin.sh" "$release_tools/harn_bin.sh"
cp -R "$repo_root/scripts/lib/." "$release_tools/lib/"

release_gate_record="$tmp_root/release-gate-record.txt"
HARN_RELEASE_ROOT="$release_root" \
  HARN_PUBLISH_SCRIPT="$release_tools/publish.sh" \
  HARN_BIN="$fake_harn" \
  HARN_BIN_NO_BUILD=1 \
  HARN_RELEASE_METADATA_BIN="$fake_release_metadata" \
  FAKE_HARN_RECORD="$release_gate_record" \
  "$repo_root/scripts/release_gate.sh" publish --dry-run > "$tmp_root/release-gate-output.txt"

expected_release=$(cat <<EOF
pwd=$release_root
arg=run
arg=--no-sandbox
arg=$release_tools/publish.harn
arg=--
arg=--dry-run
EOF
)
if [[ "$(cat "$release_gate_record")" != "$expected_release" ]]; then
  printf 'release gate did not delegate from the release root:\n%s\n' \
    "$(cat "$release_gate_record")" >&2
  exit 1
fi

echo "publish_script_test: ok"
