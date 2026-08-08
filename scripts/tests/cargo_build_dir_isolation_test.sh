#!/usr/bin/env bash
set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)

tmp_root=$(mktemp -d)
trap 'rm -rf "$tmp_root"' EXIT

fake_bin="$tmp_root/bin"
record="$tmp_root/cargo-record.txt"
mkdir -p "$fake_bin"

cat > "$fake_bin/cargo" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
{
  printf 'args=%s\n' "$*"
  printf 'CARGO_TARGET_DIR=%s\n' "${CARGO_TARGET_DIR-__unset__}"
  printf 'CARGO_BUILD_BUILD_DIR=%s\n' "${CARGO_BUILD_BUILD_DIR-__unset__}"
} >> "$FAKE_CARGO_RECORD"
case "$*" in
  tree\ *--no-default-features*)
    printf 'harn-serve\nlean-dep\n'
    ;;
  tree\ *--features\ hostlib*)
    printf 'harn-serve\nlean-dep\nhostlib-dep\n'
    ;;
  tree\ *--features\ full*)
    printf 'harn-serve\nfull-dep\nsqlx-core\ntree-sitter-a\ntree-sitter-b\ntree-sitter-c\ntree-sitter-d\ntree-sitter-e\ntree-sitter-f\ntree-sitter-g\ntree-sitter-h\ntree-sitter-i\ntree-sitter-j\ntree-sitter-k\ntree-sitter-l\ntree-sitter-m\ntree-sitter-n\ntree-sitter-o\ntree-sitter-p\ntree-sitter-q\ntree-sitter-r\ntree-sitter-s\ntree-sitter-t\ntree-sitter-u\ntree-sitter-v\ntree-sitter-w\ntree-sitter-x\ntree-sitter-y\ntree-sitter-z\ntree-sitter-aa\n'
    ;;
  build\ *)
    ;;
  *)
    echo "unexpected cargo invocation: $*" >&2
    exit 2
    ;;
esac
SH
chmod +x "$fake_bin/cargo"

PATH="$fake_bin:$PATH" \
  FAKE_CARGO_RECORD="$record" \
  "$repo_root/scripts/measure_lean_embedding.sh" --build >/dev/null

build_record="$tmp_root/cargo-build-record.txt"
awk '
  /^args=build / { in_build=1; print; next }
  /^args=/ { in_build=0 }
  in_build { print }
' "$record" > "$build_record"

if ! grep -Fq 'CARGO_BUILD_BUILD_DIR=' "$build_record"; then
  echo "measure_lean_embedding --build did not set CARGO_BUILD_BUILD_DIR" >&2
  cat "$build_record" >&2
  exit 1
fi
if grep -Fq 'CARGO_BUILD_BUILD_DIR=__unset__' "$build_record"; then
  echo "measure_lean_embedding --build left CARGO_BUILD_BUILD_DIR unset" >&2
  cat "$build_record" >&2
  exit 1
fi
if ! awk -F= '
  /^CARGO_TARGET_DIR=/ { target=$2 }
  /^CARGO_BUILD_BUILD_DIR=/ && target != "" {
    if ($2 != target "/build") {
      print "mismatched build dir: target=" target " build=" $2 > "/dev/stderr"
      exit 1
    }
    seen=1
  }
  END { exit seen ? 0 : 1 }
' "$build_record"; then
  cat "$build_record" >&2
  exit 1
fi

echo "cargo_build_dir_isolation_test: ok"
