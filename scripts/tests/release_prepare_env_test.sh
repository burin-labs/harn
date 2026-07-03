#!/usr/bin/env bash
set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)

tmp_root=$(mktemp -d)
trap 'rm -rf "$tmp_root"' EXIT

release_root="$tmp_root/release-root"
mkdir -p \
  "$release_root/crates/example" \
  "$release_root/crates/excluded" \
  "$release_root/tree-sitter-harn" \
  "$release_root/docs/src/spec/language" \
  "$release_root/docs/theme"
cat > "$release_root/Cargo.toml" <<'EOF'
[workspace]
version = "1.2.3"
members = ["crates/example", "tree-sitter-harn"]
exclude = ["crates/excluded"]
resolver = "2"
EOF
cat > "$release_root/Cargo.lock" <<'EOF'
# fake lock
EOF
cat > "$release_root/crates/example/Cargo.toml" <<'EOF'
[package]
name = "example"
version = "1.2.3"
edition = "2021"

[dependencies]
tree-sitter-harn = { path = "../../tree-sitter-harn", version = "1.2", optional = true }
harn-excluded = { path = "../excluded", version = "1.2" }
serde = { version = "1", optional = true }
EOF
cat > "$release_root/tree-sitter-harn/Cargo.toml" <<'EOF'
[package]
name = "tree-sitter-harn"
version = "1.2.3"
edition = "2021"
EOF
cat > "$release_root/crates/excluded/Cargo.toml" <<'EOF'
[package]
name = "harn-excluded"
version = "1.2.3"
edition = "2021"
EOF
cat > "$release_root/CHANGELOG.md" <<'EOF'
# Changelog

## v1.2.4

- release notes
EOF
cat > "$release_root/docs/src/embedding-rust.md" <<'EOF'
tag = "v1.2.3"
EOF
touch \
  "$release_root/docs/src/language-spec.md" \
  "$release_root/docs/src/SUMMARY.md" \
  "$release_root/docs/theme/harn-keywords.js"

git -C "$release_root" init --quiet
git -C "$release_root" config user.name "Release Test"
git -C "$release_root" config user.email "release-test@example.com"
git -C "$release_root" add .
git -C "$release_root" commit --quiet -m "initial"
git -C "$release_root" checkout --quiet -b release/v1.2.4

fake_bin="$tmp_root/bin"
mkdir -p "$fake_bin"

cat > "$fake_bin/harn" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
printf '%s\n' "$*" >> "$FAKE_HARN_RECORD"
exit 0
SH
chmod +x "$fake_bin/harn"

cat > "$fake_bin/cargo" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
echo "unexpected cargo invocation: $*" >&2
exit 2
SH
chmod +x "$fake_bin/cargo"

cat > "$fake_bin/make" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
{
  printf 'target=%s\n' "$*"
  printf 'HARN_BIN=%s\n' "${HARN_BIN-__unset__}"
  printf 'CARGO_INCREMENTAL=%s\n' "${CARGO_INCREMENTAL-__unset__}"
  printf 'RUSTC_WRAPPER=%s\n' "${RUSTC_WRAPPER-__unset__}"
  printf 'CARGO_BUILD_RUSTC_WRAPPER=%s\n' "${CARGO_BUILD_RUSTC_WRAPPER-__unset__}"
  printf 'SCCACHE_DISABLE=%s\n' "${SCCACHE_DISABLE-__unset__}"
} >> "$FAKE_MAKE_RECORD"
exit 0
SH
chmod +x "$fake_bin/make"

record_harn="$tmp_root/harn-record.txt"
record_cargo="$tmp_root/cargo-record.txt"
record_make="$tmp_root/make-record.txt"

HARN_RELEASE_ROOT="$release_root" \
HARN_BIN="$fake_bin/harn" \
FAKE_HARN_RECORD="$record_harn" \
FAKE_MAKE_RECORD="$record_make" \
PATH="$fake_bin:$PATH" \
  "$repo_root/scripts/release_gate.sh" prepare --bump minor

if ! grep -Fq "run scripts/sync_protocol_fixture_runtime_versions.harn -- --from 1.2.3 --to 1.3.0" "$record_harn"; then
  echo "release_gate prepare did not route fixture sync through HARN_BIN" >&2
  cat "$record_harn" >&2
  exit 1
fi

if ! grep -Fq 'tree-sitter-harn = { path = "../../tree-sitter-harn", version = "1.3", optional = true }' "$release_root/crates/example/Cargo.toml"; then
  echo "release_gate prepare did not update root-level workspace member dependency versions" >&2
  cat "$release_root/crates/example/Cargo.toml" >&2
  exit 1
fi
if ! grep -Fq 'harn-excluded = { path = "../excluded", version = "1.3" }' "$release_root/crates/example/Cargo.toml"; then
  echo "release_gate prepare did not preserve excluded local crate dependency version rewrites" >&2
  cat "$release_root/crates/example/Cargo.toml" >&2
  exit 1
fi
if ! grep -Fq 'serde = { version = "1", optional = true }' "$release_root/crates/example/Cargo.toml"; then
  echo "release_gate prepare should not rewrite non-local dependency versions" >&2
  cat "$release_root/crates/example/Cargo.toml" >&2
  exit 1
fi

if ! grep -Fxq "target=gen-protocol-artifacts" "$record_make"; then
  echo "release_gate prepare did not regenerate protocol artifacts" >&2
  cat "$record_make" >&2
  exit 1
fi
if ! grep -Fxq "HARN_BIN=" "$record_make"; then
  echo "release_gate prepare should force protocol artifact generation through a post-bump binary" >&2
  cat "$record_make" >&2
  exit 1
fi
if [[ -e "$record_cargo" ]]; then
  echo "release_gate prepare should not run a redundant post-bump cargo check" >&2
  cat "$record_cargo" >&2
  exit 1
fi

if ! grep -Fxq "CARGO_INCREMENTAL=0" "$record_make"; then
  echo "expected CARGO_INCREMENTAL=0 in $record_make" >&2
  cat "$record_make" >&2
  exit 1
fi
if ! grep -Fxq "RUSTC_WRAPPER=" "$record_make"; then
  echo "expected empty RUSTC_WRAPPER in $record_make" >&2
  cat "$record_make" >&2
  exit 1
fi
if ! grep -Fxq "CARGO_BUILD_RUSTC_WRAPPER=" "$record_make"; then
  echo "expected empty CARGO_BUILD_RUSTC_WRAPPER in $record_make" >&2
  cat "$record_make" >&2
  exit 1
fi
if ! grep -Fxq "SCCACHE_DISABLE=1" "$record_make"; then
  echo "expected SCCACHE_DISABLE=1 in $record_make" >&2
  cat "$record_make" >&2
  exit 1
fi

git -C "$release_root" reset --hard --quiet HEAD
: >"$record_make"

ship_gate="$tmp_root/fake-release-gate.sh"
cat > "$ship_gate" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
case "${1:-}" in
  audit)
    mkdir -p "${CARGO_TARGET_DIR:?}/debug"
    cat > "$CARGO_TARGET_DIR/debug/harn" <<'BIN'
#!/usr/bin/env bash
exit 0
BIN
    chmod +x "$CARGO_TARGET_DIR/debug/harn"
    ;;
  publish)
    ;;
  prepare)
    printf 'prepare HARN_BIN=%s\n' "${HARN_BIN-__unset__}" >> "$SHIP_GATE_RECORD"
    python3 - <<'PY'
from pathlib import Path
p = Path("Cargo.toml")
p.write_text(p.read_text().replace('version = "1.2.3"', 'version = "1.2.4"'))
lock = Path("Cargo.lock")
lock.write_text(lock.read_text() + "# touched\n")
PY
    ;;
  *)
    echo "unexpected release gate invocation: $*" >&2
    exit 2
    ;;
esac
SH
chmod +x "$ship_gate"

record_ship="$tmp_root/ship-gate-record.txt"
target_dir="$tmp_root/target"

HARN_RELEASE_ROOT="$release_root" \
HARN_RELEASE_HARNESS=1 \
HARN_RELEASE_GATE_SCRIPT="$ship_gate" \
CARGO_TARGET_DIR="$target_dir" \
SHIP_GATE_RECORD="$record_ship" \
FAKE_MAKE_RECORD="$record_make" \
PATH="$fake_bin:$PATH" \
  "$repo_root/scripts/release_ship.sh" --prepare --bump patch --skip-dry-run

expected_harn="$target_dir/debug/harn"
if ! grep -Fxq "prepare HARN_BIN=$expected_harn" "$record_ship"; then
  echo "release_ship did not pass warmed HARN_BIN into release_gate prepare" >&2
  cat "$record_ship" >&2
  exit 1
fi

echo "release_prepare_env_test: ok"
