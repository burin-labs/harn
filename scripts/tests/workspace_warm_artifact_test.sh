#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd -P)"
script="$repo_root/scripts/ci/workspace_warm_artifact.sh"
# One script now serves both platforms, so every invocation names the platform
# it is exercising. Run the suite once per platform: HARN_WARM_TEST_PLATFORM
# selects which policy block and which output-key prefix are under test.
platform="${HARN_WARM_TEST_PLATFORM:-windows}"
export HARN_RUSTC_FRONTEND_THREADS=8
tmpdir="$(mktemp -d)"
trap 'rm -rf "$tmpdir"' EXIT
if [[ "$platform" == macos ]]; then
  export RUSTC_WORKSPACE_WRAPPER="$tmpdir/parallel-workspace-wrapper"
fi

mkdir -p \
  "$tmpdir/bin" \
  "$tmpdir/work" \
  "$tmpdir/target/debug/deps" \
  "$tmpdir/target/debug/.fingerprint" \
  "$tmpdir/target/debug/incremental/foo" \
  "$tmpdir/out"
cp "$repo_root/rust-toolchain.toml" "$tmpdir/work/rust-toolchain.toml"
cp "$repo_root/scripts/tests/fixtures/df-capacity.sh" "$tmpdir/bin/df"
chmod +x "$tmpdir/bin/df"
# Keep the shell test on an isolated policy document so knobs stay config, not
# hardcoded in the assertion surface.
cp "$repo_root/.github/cache-policy.json" "$tmpdir/cache-policy.json"
export HARN_CACHE_POLICY_PATH="$tmpdir/cache-policy.json"

# Every platform-specific literal below is read back out of the policy document,
# so the assertions stay config-driven and a second platform cannot pass by
# matching the other platform's spelling.
warm_schema="$(jq -r ".${platform}_workspace_warm.manifest_schema" "$HARN_CACHE_POLICY_PATH")"
warm_artifact_name="$(jq -r ".${platform}_workspace_warm.artifact_name" "$HARN_CACHE_POLICY_PATH")"
warm_workflow="$(jq -r ".${platform}_workspace_warm.workflow" "$HARN_CACHE_POLICY_PATH")"
export FAKE_WARM_WORKFLOW="$warm_workflow"
export FAKE_WARM_ARTIFACT_NAME="$warm_artifact_name"

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
case "$*" in
  'rev-parse --verify HEAD'|'rev-parse --verify HEAD^{commit}')
    printf '%s\n' "${FAKE_COMMIT:?}"; exit 0 ;;
  'rev-parse --show-toplevel') pwd; exit 0 ;;
  'diff --quiet HEAD --'|'diff --cached --quiet HEAD --')
    [[ "${FAKE_WORKTREE_DIRTY:-false}" != true ]]; exit $? ;;
  'ls-files --others --exclude-standard -- Cargo.lock Cargo.toml crates spec tree-sitter-harn')
    exit 0 ;;
esac
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
  if [[ "$path" == *"/actions/workflows/${FAKE_WARM_WORKFLOW}/runs"* ]]; then
    printf '%s\n' "${FAKE_RUNS_JSON:?}"
    exit 0
  fi
  if [[ "$path" == *"/actions/runs/42/artifacts" ]]; then
    printf '%s\n' "${FAKE_ARTIFACTS_JSON:?}"
    exit 0
  fi
  if [[ "$path" == *"/actions/runs/43/artifacts" ]]; then
    printf '%s\n' "${FAKE_OTHER_ARTIFACTS_JSON:?}"
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
  [[ "$run_id" == "42" && "$name" == "${FAKE_WARM_ARTIFACT_NAME:?}" && -n "$dest" ]] || exit 2
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
      HARN_CACHE_POLICY_PATH="$HARN_CACHE_POLICY_PATH" \
      FAKE_TARGET="$tmpdir/target" \
      FAKE_COMMIT="$commit" \
      FAKE_RUSTC_IDENTITY="rustc 1.95.0 (fake)" \
      CARGO_TARGET_DIR="$tmpdir/target" \
      CARGO_INCREMENTAL=0 \
      RUSTFLAGS="-D warnings" \
      "$script" "$platform" pack "$tmpdir/out/staging"
  )
}

run_restore() {
  (
    cd "$tmpdir/work"
    env \
      PATH="$tmpdir/bin:$PATH" \
      HARN_CACHE_POLICY_PATH="$HARN_CACHE_POLICY_PATH" \
      FAKE_RUSTC_IDENTITY="rustc 1.95.0 (fake)" \
      CARGO_INCREMENTAL=0 \
      RUSTFLAGS="-D warnings" \
      "$script" "$platform" restore "$1" "$2"
  )
}

if [[ "$platform" == macos ]]; then
  echo "pack refuses workspace reuse from a modified checkout"
  if FAKE_WORKTREE_DIRTY=true run_pack >"$tmpdir/dirty.out" 2>&1; then
    echo "expected a dirty workspace producer to refuse" >&2
    exit 1
  fi
  grep -Fq 'changed or unmeasured source tree' "$tmpdir/dirty.out"
  test -f "$tmpdir/target/debug/deps/libharn_vm-def456.rlib"
fi
echo "pack follows the platform workspace policy and strips incremental output"
run_pack
test -f "$tmpdir/out/staging/manifest"
test -f "$tmpdir/out/staging/target.tar.gz"
if [[ "$platform" == macos ]]; then
  compiler_identity="$(sed -n 's/^workspace_compiler_sha256=//p' "$tmpdir/out/staging/manifest")"
  [[ "$compiler_identity" =~ ^[0-9a-f]{64}$ ]]
  warm_artifact_name="${warm_artifact_name}-${compiler_identity}"
  export FAKE_WARM_ARTIFACT_NAME="$warm_artifact_name"
fi

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
    HARN_CACHE_POLICY_PATH="$HARN_CACHE_POLICY_PATH" \
    FAKE_TARGET="$tmpdir/target" \
    FAKE_COMMIT="$commit" \
    FAKE_RUSTC_IDENTITY="rustc 1.95.0 (fake)" \
    CARGO_TARGET_DIR="$tmpdir/target" \
    CARGO_INCREMENTAL=0 \
    RUSTFLAGS="-D warnings" \
    "$script" "$platform" pack "$drive_staging"
)
test -f "$drive_staging/target.tar.gz"
grep -qx "schema=${warm_schema}" "$tmpdir/out/staging/manifest"
grep -qx "producer_commit=${commit}" "$tmpdir/out/staging/manifest"
grep -Eq '^target_bytes=[1-9][0-9]*$' "$tmpdir/out/staging/manifest"
if [[ "$platform" == macos ]]; then
  test -f "$tmpdir/target/debug/deps/libharn_vm-def456.rlib"
elif [[ -e "$tmpdir/target/debug/deps/libharn_vm-def456.rlib" ]]; then
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
if [[ "$platform" == macos ]]; then
  test -f "$tmpdir/restored/debug/deps/libharn_vm-def456.rlib"
  test "$(cat "$tmpdir/work/.harn-workspace-source/commit")" = "$commit"
elif [[ -e "$tmpdir/restored/debug/deps/libharn_vm-def456.rlib" ]]; then
  echo "restore unexpectedly reintroduced workspace artifacts" >&2
  exit 1
fi

echo "restore rejects insufficient build headroom before replacing a target"
if [[ "$platform" == macos ]]; then
  echo "restore refuses a forged source identity before replacing the target"
  cp -R "$tmpdir/out/staging" "$tmpdir/out/forged-source"
  sed -i.bak 's/^producer_commit=.*/producer_commit=not-a-commit/' "$tmpdir/out/forged-source/manifest"
  mkdir -p "$tmpdir/refused-source"
  printf 'preserve\n' > "$tmpdir/refused-source/sentinel"
  if run_restore "$tmpdir/out/forged-source" "$tmpdir/refused-source" >"$tmpdir/forged.out" 2>&1; then
    echo "expected forged producer identity to fail" >&2
    exit 1
  fi
  grep -Fq 'producer_commit must be a full commit SHA' "$tmpdir/forged.out"
  test -f "$tmpdir/refused-source/sentinel"
fi
rm -rf "$tmpdir/restored-tight"
mkdir -p "$tmpdir/restored-tight"
printf 'preserve\n' > "$tmpdir/restored-tight/sentinel"
if HARN_WARM_BUILD_HEADROOM_BYTES=9000000000000000000 \
  run_restore "$tmpdir/out/staging" "$tmpdir/restored-tight" \
  >"$tmpdir/tight.out" 2>"$tmpdir/tight.err"; then
  echo "expected insufficient-space admission to fail" >&2
  exit 1
fi
grep -Fxq "${platform}_warm_restore_reason=insufficient_space" "$tmpdir/tight.out"
grep -q "^${platform}_warm_restore_existing_target_bytes=" "$tmpdir/tight.out"
grep -q "^${platform}_warm_restore_available_after_replace_bytes=" "$tmpdir/tight.out"
grep -Eq "^${platform}_warm_restore_target_bytes=[1-9][0-9]*$" "$tmpdir/tight.out"
grep -Eq "^${platform}_warm_restore_headroom_bytes=[1-9][0-9]*$" "$tmpdir/tight.out"
grep -Fq 'warm artifact needs' "$tmpdir/tight.err"
test -f "$tmpdir/restored-tight/sentinel"

echo "restore distinguishes measured zero capacity from an unreadable capacity"
for available in 0 unknown; do
  if FIXTURE_DF_AVAILABLE_KIB="$available" \
    run_restore "$tmpdir/out/staging" "$tmpdir/restored-tight" \
    >"$tmpdir/capacity-$available.out" 2>"$tmpdir/capacity-$available.err"; then
    echo "expected $available capacity admission to fail" >&2
    exit 1
  fi
  if [[ "$available" == 0 ]]; then
    grep -Fxq "${platform}_warm_restore_reason=insufficient_space" "$tmpdir/capacity-$available.out"
    grep -Fxq "${platform}_warm_restore_free_bytes=0" "$tmpdir/capacity-$available.out"
  else
    grep -Fq 'could not measure filesystem free bytes' "$tmpdir/capacity-$available.err"
    if grep -Fq 'warm_restore_free_bytes=0' "$tmpdir/capacity-$available.out"; then
      echo "unreadable capacity was reported as measured zero" >&2
      exit 1
    fi
  fi
  test -f "$tmpdir/restored-tight/sentinel"
done

echo "restore rejects manifests without a typed target footprint"
rm -rf "$tmpdir/out/missing-footprint"
cp -R "$tmpdir/out/staging" "$tmpdir/out/missing-footprint"
sed -i.bak '/^target_bytes=/d' "$tmpdir/out/missing-footprint/manifest"
rm -f "$tmpdir/out/missing-footprint/manifest.bak"
if run_restore "$tmpdir/out/missing-footprint" "$tmpdir/restored-missing-footprint" \
  >"$tmpdir/missing-footprint.out" 2>"$tmpdir/missing-footprint.err"; then
  echo "expected missing target footprint to fail" >&2
  exit 1
fi
grep -Fq 'target_bytes must be a positive integer' "$tmpdir/missing-footprint.err"
test ! -e "$tmpdir/restored-missing-footprint"

echo "restore resolves a workspace-relative target before entering staging"
rm -rf "$tmpdir/work/relative-target"
(
  cd "$tmpdir/work"
  env \
    PATH="$tmpdir/bin:$PATH" \
    HARN_CACHE_POLICY_PATH="$HARN_CACHE_POLICY_PATH" \
    FAKE_RUSTC_IDENTITY="rustc 1.95.0 (fake)" \
    CARGO_INCREMENTAL=0 \
    RUSTFLAGS="-D warnings" \
    "$script" "$platform" restore "$tmpdir/out/staging" relative-target
)
test -f "$tmpdir/work/relative-target/debug/deps/libserde-abc123.rlib"

echo "restore rejects rustflags drift"
if (
  cd "$tmpdir/work"
  env \
    PATH="$tmpdir/bin:$PATH" \
    HARN_CACHE_POLICY_PATH="$HARN_CACHE_POLICY_PATH" \
    FAKE_RUSTC_IDENTITY="rustc 1.95.0 (fake)" \
    CARGO_INCREMENTAL=0 \
    RUSTFLAGS="-D warnings -Clinker=rust-lld.exe" \
    "$script" "$platform" restore "$tmpdir/out/staging" "$tmpdir/restored-bad"
); then
  echo "expected rustflags mismatch to fail" >&2
  exit 1
fi

echo "discover selects the newest successful main warm artifact run"
producer_run="$(jq -nc --arg sha "$commit" '{id:42, conclusion:"success", status:"completed", event:"push", head_branch:"main", head_repository:{full_name:"burin-labs/harn"}, head_sha:$sha}')"
FAKE_RUNS_JSON="$(jq -nc --argjson run "$producer_run" '{workflow_runs:[{id:41,conclusion:"failure"},$run,{id:43,conclusion:"success"}]}')"
export FAKE_RUNS_JSON
export FAKE_ARTIFACTS_JSON="{\"artifacts\":[{\"name\":\"${warm_artifact_name}\",\"expired\":false,\"size_in_bytes\":1234}]}"
discovered="$(
  env PATH="$tmpdir/bin:$PATH" HARN_CACHE_POLICY_PATH="$HARN_CACHE_POLICY_PATH" \
    "$script" "$platform" discover --repo burin-labs/harn
)"
[[ "$discovered" == "42" ]]

echo "discover refuses a pull-request run even when its branch is named main"
if env PATH="$tmpdir/bin:$PATH" HARN_CACHE_POLICY_PATH="$HARN_CACHE_POLICY_PATH" \
  FAKE_RUNS_JSON='{"workflow_runs":[{"id":42,"conclusion":"success","status":"completed","event":"pull_request","head_branch":"main","head_repository":{"full_name":"burin-labs/harn"},"head_sha":"0123456789abcdef0123456789abcdef01234567"}]}' \
  "$script" "$platform" discover --repo burin-labs/harn >"$tmpdir/untrusted.out" 2>&1; then
  echo "expected non-producer event to refuse" >&2
  exit 1
fi

echo "discover refuses missing, foreign, incomplete and malformed producer metadata"
invalid_runs="$(jq -nc --argjson run "$producer_run" '
  ($run | del(.event)),
  ($run | .head_repository.full_name="foreign/harn"),
  ($run | .status="in_progress"),
  ($run | .head_branch="feature"),
  ($run | .head_sha="unmeasured")
')"
refusals=0
while IFS= read -r invalid; do
  invalid_list="$(jq -nc --argjson run "$invalid" '{workflow_runs:[$run]}')"
  if env PATH="$tmpdir/bin:$PATH" HARN_CACHE_POLICY_PATH="$HARN_CACHE_POLICY_PATH" \
    FAKE_RUNS_JSON="$invalid_list" "$script" "$platform" discover --repo burin-labs/harn \
    >"$tmpdir/invalid-run.out" 2>&1; then
    echo "expected unmeasured producer metadata to refuse" >&2
    exit 1
  fi
  refusals=$((refusals + 1))
done <<< "$invalid_runs"
[[ "$refusals" == 5 ]]

echo "discover requires a measured live, nonempty artifact"
invalid_artifacts="$(jq -nc --arg name "$warm_artifact_name" '
  {artifacts:[{name:$name,size_in_bytes:1234}]},
  {artifacts:[{name:$name,expired:true,size_in_bytes:1234}]},
  {artifacts:[{name:$name,expired:false,size_in_bytes:0}]},
  {artifacts:[{name:$name,expired:false,size_in_bytes:"1234"}]}
')"
refusals=0
while IFS= read -r invalid; do
  if env PATH="$tmpdir/bin:$PATH" HARN_CACHE_POLICY_PATH="$HARN_CACHE_POLICY_PATH" \
    FAKE_ARTIFACTS_JSON="$invalid" "$script" "$platform" discover --repo burin-labs/harn \
    >"$tmpdir/invalid-artifact.out" 2>&1; then
    echo "expected unmeasured artifact metadata to refuse" >&2
    exit 1
  fi
  refusals=$((refusals + 1))
done <<< "$invalid_artifacts"
[[ "$refusals" == 4 ]]

echo "discover can stop at the first hit without breaking the JSON producer"
FAKE_RUNS_JSON="$(jq -nc --argjson run "$producer_run" '{workflow_runs: [range(0;30) | $run + {metadata:("x" * 3500)}]}')"
discovered="$(
  env PATH="$tmpdir/bin:$PATH" HARN_CACHE_POLICY_PATH="$HARN_CACHE_POLICY_PATH" \
    "$script" "$platform" discover --repo burin-labs/harn 2>"$tmpdir/discover.err"
)"
[[ "$discovered" == "42" && ! -s "$tmpdir/discover.err" ]]

echo "download-and-restore uses discover + gh run download"
rm -rf "$tmpdir/downloaded"
mkdir -p "$tmpdir/fake-artifact"
cp -R "$tmpdir/out/staging/." "$tmpdir/fake-artifact/"
export FAKE_ARTIFACT_DIR="$tmpdir/fake-artifact"
run_download() (
  cd "$tmpdir/work"
  env \
    PATH="$tmpdir/bin:$PATH" \
    HARN_CACHE_POLICY_PATH="$HARN_CACHE_POLICY_PATH" \
    FAKE_RUNS_JSON="$FAKE_RUNS_JSON" \
    FAKE_ARTIFACTS_JSON="$FAKE_ARTIFACTS_JSON" \
    FAKE_ARTIFACT_DIR="$FAKE_ARTIFACT_DIR" \
    FAKE_RUSTC_IDENTITY="rustc 1.95.0 (fake)" \
    CARGO_INCREMENTAL=0 \
    RUSTFLAGS="-D warnings" \
    "$script" "$platform" download-and-restore --repo burin-labs/harn --target-dir "$1"
)
run_download "$tmpdir/downloaded" > "$tmpdir/downloaded.out"
test -f "$tmpdir/downloaded/debug/deps/libserde-abc123.rlib"
if [[ "$platform" == macos ]]; then
  grep -Fxq "macos_warm_restore_workspace_compiler_sha256=$compiler_identity" "$tmpdir/downloaded.out"
fi

echo "download refuses a manifest from another source before replacing the target"
wrong_source="$(jq -nc --argjson run "$producer_run" '{workflow_runs:[$run + {head_sha:"1111111111111111111111111111111111111111"}]}')"
mkdir -p "$tmpdir/preserved-target"
printf 'retained\n' > "$tmpdir/preserved-target/sentinel"
if FAKE_RUNS_JSON="$wrong_source" run_download "$tmpdir/preserved-target" >"$tmpdir/source-mismatch.out" 2>&1; then
  echo "expected producer source mismatch to refuse" >&2
  exit 1
fi
grep -Fq 'producer commit does not match its GitHub run' "$tmpdir/source-mismatch.out"
[[ "$(cat "$tmpdir/preserved-target/sentinel")" == retained ]]
test ! -e "$tmpdir/preserved-target/debug/deps/libserde-abc123.rlib"

dispatch_source="$(jq -nc --argjson run "$producer_run" '{workflow_runs:[$run + {event:"workflow_dispatch",head_sha:"1111111111111111111111111111111111111111"}]}')"
if [[ "$platform" == windows ]]; then
  echo "Windows dispatch requires the source selected by its default checkout"
  matching_dispatch="$(jq -nc --argjson run "$producer_run" '{workflow_runs:[$run + {event:"workflow_dispatch"}]}')"
  FAKE_RUNS_JSON="$matching_dispatch" run_download "$tmpdir/dispatched" >"$tmpdir/dispatch.out" 2>&1
  grep -Fxq 'windows_warm_restore_source_contract=workflow_head' "$tmpdir/dispatch.out"
  test -f "$tmpdir/dispatched/debug/deps/libserde-abc123.rlib"
  if FAKE_RUNS_JSON="$dispatch_source" run_download "$tmpdir/preserved-target" >"$tmpdir/dispatch-mismatch.out" 2>&1; then
    echo "expected mismatched Windows dispatch source to refuse" >&2
    exit 1
  fi
  grep -Fq 'producer commit does not match its GitHub run' "$tmpdir/dispatch-mismatch.out"
  [[ "$(cat "$tmpdir/preserved-target/sentinel")" == retained ]]
  test ! -e "$tmpdir/preserved-target/debug/deps/libserde-abc123.rlib"
else
  echo "macOS owned dispatches are not warm artifact publishers"
  if FAKE_RUNS_JSON="$dispatch_source" run_download "$tmpdir/dispatched" >"$tmpdir/dispatch.out" 2>&1; then
    echo "expected a macOS dispatch to refuse as an archive producer" >&2
    exit 1
  fi
fi

if [[ "$platform" == macos ]]; then
  echo "a serial consumer refuses a parallel warm archive before replacing its target"
  if HARN_RUSTC_FRONTEND_THREADS=1 run_download "$tmpdir/preserved-target" >"$tmpdir/frontend-mismatch.out" 2>&1; then
    echo "expected a different compiler front end to refuse" >&2
    exit 1
  fi
  [[ "$(cat "$tmpdir/preserved-target/sentinel")" == retained ]]
  echo "restore checks the compiler identity independently of the artifact name"
  cp -R "$tmpdir/out/staging" "$tmpdir/out/forged-compiler"
  sed -i.bak 's/^workspace_compiler_sha256=.*/workspace_compiler_sha256=wrong/' "$tmpdir/out/forged-compiler/manifest"
  if run_restore "$tmpdir/out/forged-compiler" "$tmpdir/preserved-target" >"$tmpdir/forged-compiler.out" 2>&1; then
    echo "expected an unmeasured workspace compiler to refuse" >&2
    exit 1
  fi
  grep -Fq 'manifest workspace_compiler_sha256 mismatch' "$tmpdir/forged-compiler.out"
  [[ "$(cat "$tmpdir/preserved-target/sentinel")" == retained ]]
  echo "a serial consumer selects its own archive past a newer parallel producer"
  export FAKE_OTHER_ARTIFACTS_JSON="$FAKE_ARTIFACTS_JSON"
  output_file="$tmpdir/pack-outputs"
  RUSTC_WORKSPACE_WRAPPER='' HARN_RUSTC_FRONTEND_THREADS=1 GITHUB_OUTPUT="$output_file" run_pack
  serial_identity="$(sed -n 's/^workspace_compiler_sha256=//p' "$tmpdir/out/staging/manifest")"
  [[ "$serial_identity" =~ ^[0-9a-f]{64}$ && "$serial_identity" != "$compiler_identity" ]]
  base_name="$(jq -r '.macos_workspace_warm.artifact_name' "$HARN_CACHE_POLICY_PATH")"
  export FAKE_WARM_ARTIFACT_NAME="${base_name}-${serial_identity}"
  grep -Fxq "artifact_name=$FAKE_WARM_ARTIFACT_NAME" "$output_file"
  FAKE_ARTIFACTS_JSON="$(jq -nc --arg name "$FAKE_WARM_ARTIFACT_NAME" \
    '{artifacts:[{name:$name,expired:false,size_in_bytes:1234}]}')"
  FAKE_RUNS_JSON="$(jq -nc --argjson run "$producer_run" \
    '{workflow_runs:[$run + {id:43},$run]}')"
  cp -R "$tmpdir/out/staging/." "$tmpdir/fake-artifact/"
  RUSTC_WORKSPACE_WRAPPER='' HARN_RUSTC_FRONTEND_THREADS=1 run_download "$tmpdir/serial-target" > "$tmpdir/serial-target.out"
  grep -Fxq "macos_warm_restore_workspace_compiler_sha256=$serial_identity" "$tmpdir/serial-target.out"
  test -f "$tmpdir/serial-target/debug/deps/libharn_vm-def456.rlib"
fi

echo "workspace_warm_artifact_test: ok"
