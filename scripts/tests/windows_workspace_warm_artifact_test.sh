#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd -P)"
script="$repo_root/scripts/ci/windows_workspace_warm_artifact.sh"
tmpdir="$(mktemp -d)"
trap 'rm -rf "$tmpdir"' EXIT

mkdir -p \
  "$tmpdir/bin" \
  "$tmpdir/work" \
  "$tmpdir/target/debug/deps" \
  "$tmpdir/target/debug/.fingerprint" \
  "$tmpdir/target/debug/incremental/foo" \
  "$tmpdir/out"
cp "$repo_root/rust-toolchain.toml" "$tmpdir/work/rust-toolchain.toml"

# Seed third-party and workspace-shaped outputs.
printf 'dep\n' > "$tmpdir/target/debug/deps/libserde-abc123.rlib"
printf 'ws\n' > "$tmpdir/target/debug/deps/libharn_vm-def456.rlib"
printf 'ws\n' > "$tmpdir/target/debug/.fingerprint/harn-vm-def456"
printf 'inc\n' > "$tmpdir/target/debug/incremental/foo/x"

cat > "$tmpdir/bin/cargo" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
case "$1" in
  metadata)
    [[ "$*" == "metadata --format-version 1 --no-deps" ]] || exit 2
    cat <<JSON
{"target_directory":"${FAKE_TARGET:?}","packages":[{"name":"harn-vm"},{"name":"harn-cli"}]}
JSON
    ;;
  *)
    echo "unexpected cargo invocation: $*" >&2
    exit 2
    ;;
esac
SH
chmod +x "$tmpdir/bin/cargo"

cat > "$tmpdir/bin/git" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
if [[ "$*" == "rev-parse --verify HEAD" ]]; then
  printf '%s\n' "${FAKE_COMMIT:?}"
  exit 0
fi
echo "unexpected git invocation: $*" >&2
exit 2
SH
chmod +x "$tmpdir/bin/git"

cat > "$tmpdir/bin/rustc" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
[[ "$*" == "-vV" ]] || exit 2
printf '%s\n' "${FAKE_RUSTC_IDENTITY:-rustc 1.95.0 (fake)}"
SH
chmod +x "$tmpdir/bin/rustc"

cat > "$tmpdir/bin/gh" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
if [[ "$1" == "api" ]]; then
  path="$2"
  if [[ "$path" == *"/actions/workflows/windows-nightly.yml/runs"* ]]; then
    printf '%s\n' "${FAKE_RUNS_JSON:?}"
    exit 0
  fi
  if [[ "$path" == *"/actions/runs/42/artifacts" ]]; then
    printf '%s\n' "${FAKE_ARTIFACTS_JSON:?}"
    exit 0
  fi
  echo "unexpected gh api path: $path" >&2
  exit 2
fi
if [[ "$1" == "run" && "$2" == "download" ]]; then
  dest=""
  name=""
  run_id=""
  shift 2
  while (($#)); do
    case "$1" in
      --repo) shift 2 ;;
      --name) name="${2:-}"; shift 2 ;;
      --dir) dest="${2:-}"; shift 2 ;;
      *) run_id="$1"; shift ;;
    esac
  done
  [[ "$run_id" == "42" && "$name" == "workspace-windows-warm" && -n "$dest" ]] || exit 2
  mkdir -p "$dest"
  cp -R "${FAKE_ARTIFACT_DIR:?}/." "$dest/"
  exit 0
fi
echo "unexpected gh invocation: $*" >&2
exit 2
SH
chmod +x "$tmpdir/bin/gh"

commit=0123456789abcdef0123456789abcdef01234567
run_pack() {
  (
    cd "$tmpdir/work"
    env \
      PATH="$tmpdir/bin:$PATH" \
      FAKE_TARGET="$tmpdir/target" \
      FAKE_COMMIT="$commit" \
      FAKE_RUSTC_IDENTITY="rustc 1.95.0 (fake)" \
      CARGO_TARGET_DIR="$tmpdir/target" \
      CARGO_INCREMENTAL=0 \
      RUSTFLAGS="-D warnings" \
      "$script" pack "$tmpdir/out/staging"
  )
}

run_restore() {
  (
    cd "$tmpdir/work"
    env \
      PATH="$tmpdir/bin:$PATH" \
      FAKE_RUSTC_IDENTITY="rustc 1.95.0 (fake)" \
      CARGO_INCREMENTAL=0 \
      RUSTFLAGS="-D warnings" \
      "$script" restore "$1" "$2"
  )
}

echo "pack strips workspace members and incremental output"
run_pack
test -f "$tmpdir/out/staging/manifest"
test -f "$tmpdir/out/staging/target.tar.gz"

echo "pack tolerates Windows drive-letter staging paths"
drive_staging="$tmpdir/D:drive-staging"
mkdir -p "$(dirname "$drive_staging")"
# Recreate a populated target for the second pack.
mkdir -p "$tmpdir/target/debug/deps" "$tmpdir/target/debug/.fingerprint" "$tmpdir/target/debug/incremental/foo"
printf 'dep\n' > "$tmpdir/target/debug/deps/libserde-abc123.rlib"
printf 'ws\n' > "$tmpdir/target/debug/deps/libharn_vm-def456.rlib"
printf 'ws\n' > "$tmpdir/target/debug/.fingerprint/harn-vm-def456"
printf 'inc\n' > "$tmpdir/target/debug/incremental/foo/x"
(
  cd "$tmpdir/work"
  env \
    PATH="$tmpdir/bin:$PATH" \
    FAKE_TARGET="$tmpdir/target" \
    FAKE_COMMIT="$commit" \
    FAKE_RUSTC_IDENTITY="rustc 1.95.0 (fake)" \
    CARGO_TARGET_DIR="$tmpdir/target" \
    CARGO_INCREMENTAL=0 \
    RUSTFLAGS="-D warnings" \
    "$script" pack "$drive_staging"
)
test -f "$drive_staging/target.tar.gz"
grep -qx "schema=harn.windows_workspace_warm.v1" "$tmpdir/out/staging/manifest"
grep -qx "producer_commit=${commit}" "$tmpdir/out/staging/manifest"
if [[ -e "$tmpdir/target/debug/deps/libharn_vm-def456.rlib" ]]; then
  echo "workspace member artifact was not stripped" >&2
  exit 1
fi
if [[ -e "$tmpdir/target/debug/incremental" ]]; then
  echo "incremental tree was not stripped" >&2
  exit 1
fi
test -f "$tmpdir/target/debug/deps/libserde-abc123.rlib"

echo "restore replays third-party deps into an empty target"
rm -rf "$tmpdir/restored"
mkdir -p "$tmpdir/restored"
run_restore "$tmpdir/out/staging" "$tmpdir/restored"
test -f "$tmpdir/restored/debug/deps/libserde-abc123.rlib"
if [[ -e "$tmpdir/restored/debug/deps/libharn_vm-def456.rlib" ]]; then
  echo "restore unexpectedly reintroduced workspace artifacts" >&2
  exit 1
fi

echo "restore rejects rustflags drift"
if (
  cd "$tmpdir/work"
  env \
    PATH="$tmpdir/bin:$PATH" \
    FAKE_RUSTC_IDENTITY="rustc 1.95.0 (fake)" \
    CARGO_INCREMENTAL=0 \
    RUSTFLAGS="-D warnings -Clinker=rust-lld.exe" \
    "$script" restore "$tmpdir/out/staging" "$tmpdir/restored-bad"
); then
  echo "expected rustflags mismatch to fail" >&2
  exit 1
fi

echo "discover selects the newest successful main warm artifact run"
export FAKE_RUNS_JSON='{"workflow_runs":[{"id":41,"conclusion":"failure"},{"id":42,"conclusion":"success"},{"id":43,"conclusion":"success"}]}'
export FAKE_ARTIFACTS_JSON='{"artifacts":[{"name":"workspace-windows-warm","expired":false,"size_in_bytes":1234}]}'
discovered="$(
  env PATH="$tmpdir/bin:$PATH" "$script" discover --repo burin-labs/harn
)"
[[ "$discovered" == "42" ]]

echo "download-and-restore uses discover + gh run download"
rm -rf "$tmpdir/downloaded"
mkdir -p "$tmpdir/fake-artifact"
cp -R "$tmpdir/out/staging/." "$tmpdir/fake-artifact/"
export FAKE_ARTIFACT_DIR="$tmpdir/fake-artifact"
(
  cd "$tmpdir/work"
  env \
    PATH="$tmpdir/bin:$PATH" \
    FAKE_RUNS_JSON="$FAKE_RUNS_JSON" \
    FAKE_ARTIFACTS_JSON="$FAKE_ARTIFACTS_JSON" \
    FAKE_ARTIFACT_DIR="$FAKE_ARTIFACT_DIR" \
    FAKE_RUSTC_IDENTITY="rustc 1.95.0 (fake)" \
    CARGO_INCREMENTAL=0 \
    RUSTFLAGS="-D warnings" \
    "$script" download-and-restore --repo burin-labs/harn --target-dir "$tmpdir/downloaded"
)
test -f "$tmpdir/downloaded/debug/deps/libserde-abc123.rlib"

echo "windows_workspace_warm_artifact_test: ok"
