#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
tmp_root="$(mktemp -d)"
trap 'rm -rf "$tmp_root"' EXIT
fixture="$tmp_root/workspace"
bin_dir="$tmp_root/bin"
mkdir -p "$fixture/crates/example" "$bin_dir"

cat > "$fixture/Cargo.toml" <<'EOF'
[workspace]
members = ["crates/example"]
[workspace.package]
version = "1.2.3"
EOF
cat > "$fixture/crates/example/Cargo.toml" <<'EOF'
[package]
name = "example"
version.workspace = true
EOF
printf '# initial lock\n' > "$fixture/Cargo.lock"
git -C "$fixture" init --quiet
git -C "$fixture" config user.name "Development Version Test"
git -C "$fixture" config user.email "development-version-test@example.com"
git -C "$fixture" config commit.gpgsign false
git -C "$fixture" add .
git -C "$fixture" commit --quiet -m initial

cat > "$bin_dir/harn" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
printf '%s\n' "$*" >> "$FAKE_HARN_RECORD"
case "$*" in
  *"/release_metadata.harn -- current "*)
    sed -n 's/^version = "\([^"]*\)"/\1/p' "$HARN_RELEASE_ROOT/Cargo.toml"
    ;;
  *"/release_metadata.harn -- development-target "*)
    printf '1.2.4-dev\n'
    ;;
  *"/release_metadata.harn -- develop "*)
    sed 's/version = "1.2.3"/version = "1.2.4-dev"/' \
      "$HARN_RELEASE_ROOT/Cargo.toml" > "$HARN_RELEASE_ROOT/Cargo.toml.next"
    mv "$HARN_RELEASE_ROOT/Cargo.toml.next" "$HARN_RELEASE_ROOT/Cargo.toml"
    ;;
  *"/sync_protocol_fixture_runtime_versions.harn "*) ;;
  *"/sync_grammar_fitness_receipt.harn") ;;
  "dump-protocol-artifacts --artifact-version 1.2.4-dev") ;;
  *)
    echo "unexpected fake Harn invocation: $*" >&2
    exit 2
    ;;
esac
EOF
chmod +x "$bin_dir/harn"

cat > "$bin_dir/cargo" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
printf '%s\n' "$*" >> "$FAKE_CARGO_RECORD"
case "$*" in
  "metadata --format-version=1") printf '# reconciled\n' >> Cargo.lock ;;
  "metadata --format-version=1 --locked") grep -Fq '# reconciled' Cargo.lock ;;
  *) exit 2 ;;
esac
EOF
chmod +x "$bin_dir/cargo"

harn_record="$tmp_root/harn.record"
cargo_record="$tmp_root/cargo.record"
HARN_RELEASE_ROOT="$fixture" \
HARN_BIN="$bin_dir/harn" \
FAKE_HARN_RECORD="$harn_record" \
FAKE_CARGO_RECORD="$cargo_record" \
PATH="$bin_dir:$PATH" \
  "$repo_root/scripts/prepare_development_version.sh"

grep -Fq 'version = "1.2.4-dev"' "$fixture/Cargo.toml"
grep -Fxq 'metadata --format-version=1' "$cargo_record"
grep -Fxq 'metadata --format-version=1 --locked' "$cargo_record"
grep -Fq 'sync_protocol_fixture_runtime_versions.harn -- --from 1.2.3 --to 1.2.4-dev' \
  "$harn_record"
grep -Fxq 'dump-protocol-artifacts --artifact-version 1.2.4-dev' "$harn_record"
grep -Fq 'sync_grammar_fitness_receipt.harn' "$harn_record"

printf 'untracked\n' > "$fixture/UNTRACKED"
if HARN_RELEASE_ROOT="$fixture" \
  HARN_BIN="$bin_dir/harn" \
  FAKE_HARN_RECORD="$harn_record" \
  FAKE_CARGO_RECORD="$cargo_record" \
  PATH="$bin_dir:$PATH" \
    "$repo_root/scripts/prepare_development_version.sh" >/dev/null 2>&1; then
  echo "prepare_development_version accepted a dirty workspace" >&2
  exit 1
fi

echo "prepare_development_version_test: ok"
