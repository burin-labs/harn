#!/usr/bin/env bash

# Shared Harn CLI binary resolution for hooks, Make targets, and CI helper
# scripts. Cargo's binary depfile is the authority for crate inputs: it
# includes embedded assets while excluding integration tests that do not
# relink the production executable.

harn_repo_root() {
  git rev-parse --show-toplevel 2>/dev/null || pwd
}

harn_debug_bin_suffix() {
  case "${OS:-$(uname -s)}" in
    Windows_NT|MINGW*|MSYS*|CYGWIN*) printf '.exe' ;;
    *) printf '' ;;
  esac
}

harn_debug_binary_path() {
  local target_dir="${CARGO_TARGET_DIR:-}"
  if [[ -z "$target_dir" ]]; then
    target_dir="$(cargo metadata --format-version=1 --no-deps \
      | python3 -c 'import json, sys; print(json.load(sys.stdin)["target_directory"])')"
  fi
  printf '%s/debug/harn%s\n' "$target_dir" "$(harn_debug_bin_suffix)"
}

harn_bin_newer_source_report() {
  local bin="$1"
  python3 - "$bin" <<'PY'
import os
import subprocess
import sys

bin_path = sys.argv[1]
try:
    bin_mtime = os.stat(bin_path).st_mtime_ns
except FileNotFoundError:
    print(f"{bin_path} does not exist")
    sys.exit(1)

try:
    root = subprocess.check_output(
        ["git", "rev-parse", "--show-toplevel"],
        text=True,
        stderr=subprocess.DEVNULL,
    ).strip()
except subprocess.CalledProcessError:
    sys.exit(0)

global_pathspecs = [
    "Cargo.lock",
    "Cargo.toml",
    "rust-toolchain",
    "rust-toolchain.toml",
    ".cargo/config",
    ".cargo/config.toml",
]
proc = subprocess.run(
    ["git", "-C", root, "ls-files", "-z", "--", *global_pathspecs],
    check=True,
    stdout=subprocess.PIPE,
)


def makefile_words(value):
    words = []
    current = []
    escaped = False
    for char in value:
        if escaped:
            current.append(char)
            escaped = False
        elif char == "\\":
            escaped = True
        elif char.isspace():
            if current:
                words.append("".join(current))
                current = []
        else:
            current.append(char)
    if escaped:
        current.append("\\")
    if current:
        words.append("".join(current))
    return words


depfile_name = os.path.basename(bin_path)
if depfile_name.lower().endswith(".exe"):
    depfile_name = depfile_name[:-4]
depfile = os.path.join(os.path.dirname(bin_path), depfile_name + ".d")
dependencies = []
try:
    with open(depfile, encoding="utf-8", errors="surrogateescape") as handle:
        depfile_text = handle.read().replace("\\\n", "")
    separator = next(
        (index for index in range(len(depfile_text) - 1)
         if depfile_text[index] == ":" and depfile_text[index + 1].isspace()),
        -1,
    )
    if separator >= 0:
        dependencies = makefile_words(depfile_text[separator + 1:])
except FileNotFoundError:
    # Copied/custom binaries lack Cargo metadata. Keep their fallback
    # conservative, but exclude source trees Cargo never links into `harn`.
    fallback = subprocess.run(
        [
            "git", "-C", root, "ls-files", "-z", "--", "crates",
            ":(exclude,glob)crates/**/tests/**",
            ":(exclude,glob)crates/**/benches/**",
            ":(exclude,glob)crates/**/examples/**",
        ],
        check=True,
        stdout=subprocess.PIPE,
    )
    dependencies = [
        os.path.join(root, raw.decode("utf-8", "surrogateescape"))
        for raw in fallback.stdout.split(b"\0") if raw
    ]

for raw in proc.stdout.split(b"\0"):
    if raw:
        dependencies.append(os.path.join(root, raw.decode("utf-8", "surrogateescape")))

newer = []
seen = set()
root_real = os.path.realpath(root)
for dependency in dependencies:
    path = dependency if os.path.isabs(dependency) else os.path.join(root, dependency)
    path = os.path.normpath(path)
    if path in seen:
        continue
    seen.add(path)
    path_real = os.path.realpath(path)
    try:
        inside_root = os.path.commonpath([root_real, path_real]) == root_real
    except ValueError:
        inside_root = False
    rel = os.path.relpath(path_real, root_real) if inside_root else path
    try:
        if os.stat(path).st_mtime_ns > bin_mtime:
            newer.append(rel)
    except FileNotFoundError:
        newer.append(rel + " (missing)")

if newer:
    for rel in newer[:12]:
        print(rel)
    if len(newer) > 12:
        print(f"... and {len(newer) - 12} more")
    sys.exit(1)
PY
}

harn_bin_is_fresh() {
  local bin="$1"
  [[ -x "$bin" ]] || return 1
  [[ "${HARN_BIN_ASSUME_FRESH:-0}" = "1" ]] && return 0
  harn_bin_newer_source_report "$bin" >/dev/null
}

harn_require_fresh_bin() {
  local bin="$1"
  if [[ ! -x "$bin" ]]; then
    echo "error: HARN_BIN is not executable: $bin" >&2
    return 1
  fi
  if [[ "${HARN_BIN_ASSUME_FRESH:-0}" = "1" ]]; then
    return 0
  fi
  local stale
  if ! stale="$(harn_bin_newer_source_report "$bin")"; then
    echo "error: harn binary is stale relative to compiled executable inputs: $bin" >&2
    if [[ -n "$stale" ]]; then
      echo "newer inputs:" >&2
      printf '%s\n' "$stale" | sed 's/^/  /' >&2
    fi
    echo "hint: run scripts/ci_warm_harn_bin.sh or unset HARN_BIN to rebuild." >&2
    return 1
  fi
}

harn_resolve_binary() {
  local mode="${1:-build}"
  local bin=""

  if [[ -n "${HARN_BIN:-}" ]]; then
    if ! harn_require_fresh_bin "$HARN_BIN"; then
      return 1
    fi
    printf '%s\n' "$HARN_BIN"
    return 0
  fi

  if [[ -z "${CARGO_TARGET_DIR:-}" ]]; then
    CARGO_TARGET_DIR="$(cargo metadata --format-version=1 --no-deps \
      | python3 -c 'import json, sys; print(json.load(sys.stdin)["target_directory"])')"
    export CARGO_TARGET_DIR
  fi
  harn_export_cargo_build_dir_for_target "$CARGO_TARGET_DIR" || true

  bin="$(harn_debug_binary_path)"
  if harn_bin_is_fresh "$bin"; then
    printf '%s\n' "$bin"
    return 0
  fi

  if [[ "$mode" = "no-build" ]]; then
    echo "error: no fresh worktree harn binary found at $bin" >&2
    echo "hint: run scripts/ci_warm_harn_bin.sh, then retry." >&2
    return 1
  fi

  cargo build --quiet --bin harn
  bin="$(harn_debug_binary_path)"
  if ! harn_require_fresh_bin "$bin"; then
    return 1
  fi
  printf '%s\n' "$bin"
}
