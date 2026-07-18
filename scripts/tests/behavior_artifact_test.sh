#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd -P)"
script="$repo_root/scripts/ci/behavior_artifact.sh"
tmpdir="$(mktemp -d)"
trap 'rm -rf "$tmpdir"' EXIT

mkdir -p "$tmpdir/bin" "$tmpdir/target/debug" "$tmpdir/out" "$tmpdir/receipts"
cat > "$tmpdir/bin/cargo" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
case "$1" in
  build)
    if [[ "$#" -ne 4 || "$2" != "--locked" || "$3" != "--bin" || "$4" != "harn" ]]; then
      printf 'unexpected cargo build argv:' >&2
      printf ' <%s>' "$@" >&2
      printf '\n' >&2
      exit 2
    fi
    : > "${CARGO_RECEIPTS:?}/build"
    ;;
  metadata)
    if [[ "$#" -ne 4 || "$2" != "--format-version" || "$3" != "1" || "$4" != "--no-deps" ]]; then
      printf 'unexpected cargo metadata argv:' >&2
      printf ' <%s>' "$@" >&2
      printf '\n' >&2
      exit 2
    fi
    printf '{"target_directory":"%s"}\n' "${FAKE_TARGET:?}"
    ;;
  nextest)
    if [[ "$#" -eq 2 && "$2" == "--version" ]]; then
      cat <<'VERSION'
cargo-nextest 0.9.132 (6e4a9d6f2 2026-03-20)
release: 0.9.132
commit-hash: 6e4a9d6f2c4964f30ff54a8cd5466f8869267daa
VERSION
      exit 0
    fi
    if [[ "$#" -ne 10 || "$2" != "archive" || "$3" != "--locked" || \
      "$4" != "--workspace" || "$5" != "--profile" || "$6" != "ci" || \
      "$7" != "-E" || "$8" != "package(harn-vm) and binary(harn_vm)" || \
      "$9" != "--archive-file" || -z "${10}" ]]; then
      printf 'unexpected cargo nextest argv:' >&2
      printf ' <%s>' "$@" >&2
      printf '\n' >&2
      exit 2
    fi
    output=${10}
    printf 'security archive\n' > "${output:?missing --archive-file}"
    : > "${CARGO_RECEIPTS:?}/nextest-archive"
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
printf '#!/usr/bin/env bash\necho harn\n' > "$tmpdir/target/debug/harn"
chmod +x "$tmpdir/target/debug/harn"

commit=0123456789abcdef0123456789abcdef01234567
bundle="$tmpdir/out/behavior.tar.zst"
PATH="$tmpdir/bin:$PATH" CARGO_RECEIPTS="$tmpdir/receipts" FAKE_TARGET="$tmpdir/target" FAKE_COMMIT="$commit" \
  "$script" build "$bundle" "$commit"
test -f "$tmpdir/receipts/build"
test -f "$tmpdir/receipts/nextest-archive"

github_env="$tmpdir/github-env"
PATH="$tmpdir/bin:$PATH" CARGO_RECEIPTS="$tmpdir/receipts" FAKE_TARGET="$tmpdir/target" FAKE_COMMIT="$commit" \
  "$script" restore "$bundle" "$tmpdir/restored" "$commit" "$github_env"
"$tmpdir/restored/harn" | grep -Fxq harn
restored="$(cd "$tmpdir/restored" && pwd -P)"
grep -Fxq "HARN_BIN=$restored/harn" "$github_env"

set +e
PATH="$tmpdir/bin:$PATH" CARGO_RECEIPTS="$tmpdir/receipts" FAKE_TARGET="$tmpdir/target" FAKE_COMMIT="$commit" \
  "$script" restore "$bundle" "$tmpdir/wrong-commit" \
  fedcba9876543210fedcba9876543210fedcba98 >/dev/null 2>&1
status=$?
set -e
if [[ "$status" -eq 0 ]]; then
  echo "restore accepted a bundle for the wrong commit" >&2
  exit 1
fi

set +e
PATH="$tmpdir/bin:$PATH" CARGO_RECEIPTS="$tmpdir/receipts" FAKE_TARGET="$tmpdir/target" \
  FAKE_COMMIT=fedcba9876543210fedcba9876543210fedcba98 \
  "$script" build "$tmpdir/out/wrong-source.tar.zst" "$commit" >/dev/null 2>&1
status=$?
set -e
if [[ "$status" -eq 0 ]]; then
  echo "build accepted a commit that did not match checkout HEAD" >&2
  exit 1
fi

echo "behavior_artifact_test: ok"
