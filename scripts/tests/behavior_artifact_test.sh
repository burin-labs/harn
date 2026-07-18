#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd -P)"
script="$repo_root/scripts/ci/behavior_artifact.sh"
tmpdir="$(mktemp -d)"
trap 'rm -rf "$tmpdir"' EXIT

mkdir -p "$tmpdir/bin" "$tmpdir/target/debug" "$tmpdir/out"
record="$tmpdir/cargo-args"
cat > "$tmpdir/bin/cargo" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
printf '%s\n' "$*" >> "${CARGO_RECORD:?}"
case "$1" in
  build)
    ;;
  metadata)
    printf '{"target_directory":"%s"}\n' "${FAKE_TARGET:?}"
    ;;
  nextest)
    if [[ "$2" == "--version" ]]; then
      echo "cargo-nextest 0.9.133"
      exit 0
    fi
    output=""
    while [[ $# -gt 0 ]]; do
      if [[ "$1" == "--archive-file" ]]; then
        output=$2
        break
      fi
      shift
    done
    printf 'security archive\n' > "${output:?missing --archive-file}"
    ;;
  *)
    echo "unexpected cargo invocation: $*" >&2
    exit 2
    ;;
esac
SH
chmod +x "$tmpdir/bin/cargo"
printf '#!/usr/bin/env bash\necho harn\n' > "$tmpdir/target/debug/harn"
chmod +x "$tmpdir/target/debug/harn"

commit=0123456789abcdef0123456789abcdef01234567
bundle="$tmpdir/out/behavior.tar.zst"
PATH="$tmpdir/bin:$PATH" CARGO_RECORD="$record" FAKE_TARGET="$tmpdir/target" \
  "$script" build "$bundle" "$commit"
grep -Fq \
  'build --locked --bin harn' \
  "$record"
grep -Fq \
  'nextest archive --locked --workspace --profile ci -E package(harn-vm) and binary(harn_vm) --archive-file ' \
  "$record"

github_env="$tmpdir/github-env"
PATH="$tmpdir/bin:$PATH" CARGO_RECORD="$record" FAKE_TARGET="$tmpdir/target" \
  "$script" restore "$bundle" "$tmpdir/restored" "$commit" "$github_env"
"$tmpdir/restored/harn" | grep -Fxq harn
restored="$(cd "$tmpdir/restored" && pwd -P)"
grep -Fxq "HARN_BIN=$restored/harn" "$github_env"

set +e
PATH="$tmpdir/bin:$PATH" CARGO_RECORD="$record" FAKE_TARGET="$tmpdir/target" \
  "$script" restore "$bundle" "$tmpdir/wrong-commit" \
  fedcba9876543210fedcba9876543210fedcba98 >/dev/null 2>&1
status=$?
set -e
if [[ "$status" -eq 0 ]]; then
  echo "restore accepted a bundle for the wrong commit" >&2
  exit 1
fi

echo "behavior_artifact_test: ok"
