#!/usr/bin/env bash

# Freshness proof for auto-resolved worktree Harn executables. Callers use the
# two bottom-level functions: record after Cargo successfully resolves a build,
# and require before no-build execution. Cargo owns the dependency set, a typed
# parser in the exact Harn artifact owns depfile syntax and content identity,
# and Git owns checkout content identity.

harn_binary_dep_info_path() {
  local bin="$1"
  local directory="${bin%/*}"
  local name="${bin##*/}"
  name="${name%.exe}"
  printf '%s/%s.d\n' "$directory" "$name"
}

harn_binary_freshness_receipt_path() {
  printf '%s.freshness\n' "$1"
}

harn_binary_freshness_manifest_path() {
  printf '%s.freshness.manifest\n' "$1"
}

harn_cargo_freshness_checker_path() {
  local bin="$1"
  local suffix=""
  case "$bin" in
    *.exe) suffix=".exe" ;;
  esac
  printf '%s/harn-freshness-check%s\n' "${bin%/*}" "$suffix"
}

# The proof checker is a producer-owned snapshot, not Cargo's mutable top-level
# output. Later test-profile commands may legitimately relink
# harn-freshness-check while retaining the exact Harn executable; receipt
# verification must bind the checker that was actually published, not an
# adjacent build output whose lifecycle Cargo still owns.
harn_binary_freshness_checker_path() {
  local bin="$1"
  local directory="${bin%/*}"
  local name="${bin##*/}"
  local suffix=""
  case "$name" in
    *.exe)
      name="${name%.exe}"
      suffix=".exe"
      ;;
  esac
  printf '%s/%s.freshness-check%s\n' "$directory" "$name" "$suffix"
}

harn_binary_target_dir() {
  local bin="$1"
  local profile_dir="${bin%/*}"
  printf '%s\n' "${profile_dir%/*}"
}

harn_collect_artifact_freshness_evidence() {
  local bin="$1"
  local git_covered_list="$2"
  local authority_list="${3:-}"
  local manifest="${4:-}"
  local dep_info=""
  dep_info="$(harn_binary_dep_info_path "$bin")" || return $?
  if [[ -n "$authority_list" && -n "$manifest" ]]; then
    "$bin" __internal-freshness-evidence-v5 \
      "$dep_info" "$bin" "$(harn_repo_root)" "$git_covered_list" \
      "$authority_list" "$manifest"
  else
    "$bin" __internal-freshness-evidence-v5 \
      "$dep_info" "$bin" "$(harn_repo_root)" "$git_covered_list"
  fi
}

harn_write_freshness_authority_path() {
  local output="$1"
  local path="$2"
  local projected="$path"

  # Authority-list entries cross a data boundary: unlike command-line
  # arguments, NUL-delimited file contents are not rewritten by MSYS before a
  # native Windows Harn process reads them. Preserve POSIX paths for shell
  # identity/existence checks, then project only the serialized consumer path.
  case "${OS:-$(uname -s)}" in
    Windows_NT | MINGW* | MSYS* | CYGWIN*)
      if ! command -v cygpath >/dev/null 2>&1; then
        echo "error: cannot project Harn manifest authority to a native Windows path without cygpath" >&2
        return 1
      fi
      projected="$(cygpath -w "$path")" || return $?
      ;;
  esac
  printf '%s\0' "$projected" >>"$output"
}

harn_write_freshness_authority_list() {
  local output="$1"
  local repo_root=""
  local path=""
  local head_ref=""

  repo_root="$(harn_repo_root)" || return $?
  : >"$output" || return $?
  path="$(git -C "$repo_root" rev-parse --path-format=absolute --git-path HEAD)" || return $?
  if [[ -f "$path" ]]; then
    harn_write_freshness_authority_path "$output" "$path" || return $?
  fi
  head_ref="$(git -C "$repo_root" symbolic-ref -q HEAD 2>/dev/null || true)"
  if [[ -n "$head_ref" ]]; then
    path="$(git -C "$repo_root" rev-parse --path-format=absolute --git-path "$head_ref")" || return $?
    if [[ -f "$path" ]]; then
      harn_write_freshness_authority_path "$output" "$path" || return $?
    else
      path="$(git -C "$repo_root" rev-parse --path-format=absolute --git-path packed-refs)" || return $?
      if [[ -f "$path" ]]; then
        harn_write_freshness_authority_path "$output" "$path" || return $?
      fi
    fi
  fi
  for path in \
    "$repo_root/.cargo" \
    "$repo_root/.cargo/config.toml" \
    "$(git -C "$repo_root" rev-parse --path-format=absolute --git-path config)" \
    "$(git -C "$repo_root" rev-parse --path-format=absolute --git-path info/exclude)"; do
    if [[ -e "$path" ]]; then
      harn_write_freshness_authority_path "$output" "$path" || return $?
    fi
  done
}

harn_build_freshness_id_from_parts() {
  local worktree_hash="$1"
  local dep_info_hash="$2"
  local dependencies_hash="$3"
  printf 'harn-build-freshness-v1\nworktree=%s\ndep-info=%s\ndependencies=%s\n' \
    "$worktree_hash" "$dep_info_hash" "$dependencies_hash" \
    | git -C "$(harn_repo_root)" hash-object --stdin
}

harn_freshness_input_state() {
  local path="$1"
  local executable="${2:-0}"

  if [[ ! -e "$path" && ! -L "$path" ]]; then
    printf 'missing\n'
  elif [[ ! -f "$path" ]]; then
    printf 'non-regular\n'
  elif [[ "$executable" = "1" && ! -x "$path" ]]; then
    printf 'regular-not-executable\n'
  elif [[ ! -r "$path" ]]; then
    printf 'regular-not-readable\n'
  elif [[ "$executable" = "1" ]]; then
    printf 'regular-readable-executable\n'
  else
    printf 'regular-readable\n'
  fi
}

harn_report_artifact_freshness_failure() {
  local bin="$1"
  local dep_info="$2"
  local producer_error="$3"
  local line=""
  local line_count=0
  local LC_ALL=C

  echo "error: cannot compute post-build Harn freshness identity from Cargo dep-info" >&2
  printf 'error: Harn freshness artifact state: binary=%s path=%q; dep-info=%s path=%q\n' \
    "$(harn_freshness_input_state "$bin" 1)" "$bin" \
    "$(harn_freshness_input_state "$dep_info")" "$dep_info" >&2
  if [[ ! -s "$producer_error" ]]; then
    echo "error: Harn artifact evidence producer exited without a diagnostic" >&2
    return
  fi
  while IFS= read -r line || [[ -n "$line" ]]; do
    printf 'error: Harn artifact evidence producer: %.1024s\n' "$line" >&2
    line_count=$((line_count + 1))
    [[ "$line_count" -lt 4 ]] || break
  done <"$producer_error"
}

# Compute the exact semantic input identity Cargo embeds into the next Harn
# link. Git owns source/provenance content; Cargo dep-info owns ignored,
# generated, and external prerequisites. On a first build there is no prior
# artifact capable of decoding dep-info, so a versioned bootstrap marker forces
# the initial link; the post-build fixed-point check below then converges once
# the authoritative dep-info exists.
harn_build_freshness_id() (
  local bin="$1"
  local require_artifact="${2:-0}"
  local target_dir=""
  local git_covered_list=""
  local worktree_hash=""
  local artifact_evidence=""
  local artifact_error=""
  local dep_info=""
  local dep_info_hash="bootstrap"
  local dependencies_hash="bootstrap"

  cleanup_git_covered_list() {
    [[ -z "$git_covered_list" ]] || rm -f "$git_covered_list"
    [[ -z "$artifact_error" ]] || rm -f "$artifact_error"
  }
  trap cleanup_git_covered_list EXIT
  trap 'exit 129' HUP
  trap 'exit 130' INT
  trap 'exit 143' TERM

  target_dir="$(harn_binary_target_dir "$bin")" || return $?
  dep_info="$(harn_binary_dep_info_path "$bin")" || return $?
  git_covered_list="$(mktemp "${TMPDIR:-/tmp}/harn-bin-git-covered.XXXXXX")" || return $?
  artifact_error="$(mktemp "${TMPDIR:-/tmp}/harn-bin-artifact-error.XXXXXX")" || return $?
  worktree_hash="$(harn_worktree_content_fingerprint "$target_dir" "$git_covered_list")" || return $?
  if [[ -x "$bin" ]] && artifact_evidence="$(
    harn_collect_artifact_freshness_evidence "$bin" "$git_covered_list" 2>"$artifact_error"
  )"; then
    dep_info_hash="$(printf '%s\n' "$artifact_evidence" | sed -n '5s/^dep-info=//p')"
    dependencies_hash="$(printf '%s\n' "$artifact_evidence" | sed -n '6s/^dependencies=//p')"
  elif [[ "$require_artifact" = "1" ]]; then
    harn_report_artifact_freshness_failure "$bin" "$dep_info" "$artifact_error"
    return 1
  fi
  if [[ ! "$dep_info_hash" =~ ^(bootstrap|[0-9a-f]{64})$ ]] || \
     [[ ! "$dependencies_hash" =~ ^(bootstrap|[0-9a-f]{64})$ ]]; then
    echo "error: malformed Cargo dependency evidence while computing Harn build freshness" >&2
    return 1
  fi
  harn_build_freshness_id_from_parts \
    "$worktree_hash" "$dep_info_hash" "$dependencies_hash"
)

harn_embedded_build_freshness_id() (
  local bin="$1"
  local target_dir=""
  local git_covered_list=""
  local artifact_evidence=""

  cleanup_git_covered_list() {
    [[ -z "$git_covered_list" ]] || rm -f "$git_covered_list"
  }
  trap cleanup_git_covered_list EXIT
  target_dir="$(harn_binary_target_dir "$bin")" || return $?
  git_covered_list="$(mktemp "${TMPDIR:-/tmp}/harn-bin-git-covered.XXXXXX")" || return $?
  harn_worktree_content_fingerprint "$target_dir" "$git_covered_list" >/dev/null || return $?
  artifact_evidence="$(harn_collect_artifact_freshness_evidence "$bin" "$git_covered_list")" || return $?
  printf '%s\n' "$artifact_evidence" | sed -n '2s/^build-freshness=//p'
)

# Hash the exact Git-owned checkout state without printing source content. Git's
# index/status/diff shortcuts are metadata-based and therefore cannot prove
# same-size bytes when timestamps are restored. Use Git's object encoder once
# in line-oriented batch mode for regular-file bytes; explicitly frame paths,
# worktree mode, missing tracked inputs, and exact symlink targets around those
# object IDs. Newlines cannot be represented by `--stdin-paths`, so those rare
# paths fail closed instead of falling back to a heuristic or per-file process.
#
# The target directory is excluded from untracked inputs because it is build
# output and may be caller-configured inside the checkout. Tracked files there
# remain authoritative like every other tracked path.
harn_worktree_content_fingerprint() (
  local target_dir="$1"
  local git_covered_list="$2"
  local repo_root=""
  local head=""
  local path=""
  local absolute_path=""
  local tracked_list=""
  local untracked_list=""
  local content_paths=""
  local content_hashes=""
  local records=""

  cleanup_worktree_fingerprint_files() {
    [[ -z "$tracked_list" ]] || rm -f "$tracked_list"
    [[ -z "$untracked_list" ]] || rm -f "$untracked_list"
    [[ -z "$content_paths" ]] || rm -f "$content_paths"
    [[ -z "$content_hashes" ]] || rm -f "$content_hashes"
    [[ -z "$records" ]] || rm -f "$records"
  }

  append_worktree_path() {
    local scope="$1"
    local relative="$2"
    local mode=""
    local target_with_sentinel=""
    local target=""

    case "$relative" in
      *$'\n'*)
        echo "error: cannot exactly fingerprint Git path containing a newline: $relative" >&2
        return 1
        ;;
    esac
    printf '%s\0' "$relative" >>"$git_covered_list" || return $?
    absolute_path="$repo_root/$relative"
    if [[ -L "$absolute_path" ]]; then
      target_with_sentinel="$(readlink "$absolute_path"; printf '.')" || return $?
      target="${target_with_sentinel%.}"
      printf '%s\0%s\0symlink\0-\0%s\0' \
        "$scope" "$relative" "$target" >>"$records" || return $?
    elif [[ -f "$absolute_path" ]]; then
      mode="data"
      [[ -x "$absolute_path" ]] && mode="executable"
      printf '%s\0%s\0file\0%s\0hash\0' \
        "$scope" "$relative" "$mode" >>"$records" || return $?
      printf '%s\n' "$relative" >>"$content_paths" || return $?
    elif [[ ! -e "$absolute_path" && "$scope" = "tracked" ]]; then
      printf '%s\0%s\0missing\0-\0-\0' \
        "$scope" "$relative" >>"$records" || return $?
    else
      echo "error: Git-owned worktree input is not a regular file or symlink: $relative" >&2
      return 1
    fi
  }

  export LC_ALL=C
  set -o pipefail
  trap cleanup_worktree_fingerprint_files EXIT
  trap 'exit 129' HUP
  trap 'exit 130' INT
  trap 'exit 143' TERM

  repo_root="$(harn_repo_root)" || return $?
  head="$(git -C "$repo_root" rev-parse --verify HEAD)" || return $?
  target_dir="$(cd "$target_dir" 2>/dev/null && pwd -P)" || return $?
  tracked_list="$(mktemp "${TMPDIR:-/tmp}/harn-bin-tracked.XXXXXX")" || return $?
  untracked_list="$(mktemp "${TMPDIR:-/tmp}/harn-bin-untracked.XXXXXX")" || return $?
  content_paths="$(mktemp "${TMPDIR:-/tmp}/harn-bin-content-paths.XXXXXX")" || return $?
  content_hashes="$(mktemp "${TMPDIR:-/tmp}/harn-bin-content-hashes.XXXXXX")" || return $?
  records="$(mktemp "${TMPDIR:-/tmp}/harn-bin-content-records.XXXXXX")" || return $?
  : >"$git_covered_list" || return $?
  git -C "$repo_root" ls-files --cached -z >"$tracked_list" || return $?
  git -C "$repo_root" ls-files --others --exclude-standard -z >"$untracked_list" || return $?
  while IFS= read -r -d '' path; do
    append_worktree_path tracked "$path" || return $?
  done <"$tracked_list"
  while IFS= read -r -d '' path; do
    absolute_path="$repo_root/$path"
    case "$absolute_path" in
      "$target_dir"|"$target_dir"/*) continue ;;
    esac
    append_worktree_path untracked "$path" || return $?
  done <"$untracked_list"
  git -C "$repo_root" hash-object --no-filters --stdin-paths \
    <"$content_paths" >"$content_hashes" || return $?

  {
    local scope=""
    local kind=""
    local mode=""
    local detail=""
    local object_id=""
    exec 3<"$content_hashes"
    printf 'harn-worktree-content-v2\0head\0%s\0' "$head"
    while IFS= read -r -d '' scope && \
          IFS= read -r -d '' path && \
          IFS= read -r -d '' kind && \
          IFS= read -r -d '' mode && \
          IFS= read -r -d '' detail; do
      printf '%s\0%s\0%s\0%s\0' "$scope" "$path" "$kind" "$mode"
      if [[ "$detail" = "hash" ]]; then
        IFS= read -r object_id <&3 || {
          echo "error: Git content hash batch ended before its path inventory" >&2
          exit 1
        }
        if [[ ! "$object_id" =~ ^([0-9a-f]{40}|[0-9a-f]{64})$ ]]; then
          echo "error: Git content hash batch returned a malformed object ID" >&2
          exit 1
        fi
        printf '%s\0' "$object_id"
      else
        printf '%s\0' "$detail"
      fi
    done <"$records"
    if IFS= read -r object_id <&3; then
      echo "error: Git content hash batch exceeded its path inventory" >&2
      exit 1
    fi
    exec 3<&-
  } | git -C "$repo_root" hash-object --stdin
)

harn_print_binary_freshness_recovery() {
  echo "hint: rebuild and refresh the receipt with ./scripts/harn_bin.sh --print" >&2
  echo "hint: or intentionally pin caller-owned evidence with HARN_BIN=<path-to-harn>" >&2
}

harn_record_binary_freshness() (
  local bin="$1"
  local receipt=""
  local manifest=""
  local checker=""
  local cargo_checker=""
  local temporary_checker=""
  local temporary_receipt=""
  local temporary_manifest=""
  local worktree_hash=""
  local target_dir=""
  local artifact_evidence=""
  local checker_evidence=""
  local git_covered_list=""
  local authority_list=""
  local expected_build_freshness=""
  local embedded_build_freshness=""
  local dep_info_hash=""
  local dependencies_hash=""

  cleanup_temporary_files() {
    [[ -z "$temporary_receipt" ]] || rm -f "$temporary_receipt"
    [[ -z "$temporary_manifest" ]] || rm -f "$temporary_manifest"
    [[ -z "$temporary_checker" ]] || rm -f "$temporary_checker"
    [[ -z "$git_covered_list" ]] || rm -f "$git_covered_list"
    [[ -z "$authority_list" ]] || rm -f "$authority_list"
  }
  trap cleanup_temporary_files EXIT
  trap 'exit 129' HUP
  trap 'exit 130' INT
  trap 'exit 143' TERM

  harn_require_executable_bin "$bin" || return $?
  receipt="$(harn_binary_freshness_receipt_path "$bin")" || return $?
  manifest="$(harn_binary_freshness_manifest_path "$bin")" || return $?
  cargo_checker="$(harn_cargo_freshness_checker_path "$bin")" || return $?
  harn_require_executable_bin "$cargo_checker" || return $?
  checker="$(harn_binary_freshness_checker_path "$bin")" || return $?
  target_dir="$(harn_binary_target_dir "$bin")" || return $?
  git_covered_list="$(mktemp "${TMPDIR:-/tmp}/harn-bin-git-covered.XXXXXX")" || return $?
  authority_list="$(mktemp "${TMPDIR:-/tmp}/harn-bin-authorities.XXXXXX")" || return $?
  temporary_manifest="$(mktemp "${manifest}.tmp.XXXXXX")" || return $?
  worktree_hash="$(harn_worktree_content_fingerprint "$target_dir" "$git_covered_list")" || return $?
  harn_write_freshness_authority_list "$authority_list" || return $?
  artifact_evidence="$(harn_collect_artifact_freshness_evidence \
    "$bin" "$git_covered_list" "$authority_list" "$temporary_manifest")" || return $?
  embedded_build_freshness="$(printf '%s\n' "$artifact_evidence" | sed -n '2s/^build-freshness=//p')"
  dep_info_hash="$(printf '%s\n' "$artifact_evidence" | sed -n '5s/^dep-info=//p')"
  dependencies_hash="$(printf '%s\n' "$artifact_evidence" | sed -n '6s/^dependencies=//p')"
  expected_build_freshness="$(harn_build_freshness_id_from_parts \
    "$worktree_hash" "$dep_info_hash" "$dependencies_hash")" || return $?
  if [[ "$embedded_build_freshness" != "$expected_build_freshness" ]]; then
    echo "error: cannot record Harn freshness: compiled build identity does not match current source and Cargo inputs" >&2
    return 1
  fi
  temporary_checker="$(mktemp "${checker}.tmp.XXXXXX")" || return $?
  cp "$cargo_checker" "$temporary_checker" || return $?
  chmod +x "$temporary_checker" || return $?
  mv "$temporary_checker" "$checker" || return $?
  temporary_checker=""
  checker_evidence="$("$checker" record-evidence \
    "$bin" "$temporary_manifest" "$(harn_repo_root)")" || return $?
  temporary_receipt="$(mktemp "${receipt}.tmp.XXXXXX")" || return $?
  printf 'harn-bin-freshness-v4\nworktree=%s\n%s\n%s\n' \
    "$worktree_hash" "$artifact_evidence" "$checker_evidence" >"$temporary_receipt" || return $?
  mv "$temporary_manifest" "$manifest" || return $?
  temporary_manifest=""
  mv "$temporary_receipt" "$receipt" || return $?
  temporary_receipt=""
  # Close the producer's final TOCTOU window before handing this artifact to a
  # caller. The checker rebinds the now-canonical receipt/manifest pair and
  # rechecks both executable identities after the atomic same-directory moves.
  "$checker" verify "$receipt" "$manifest" "$bin" "$(harn_repo_root)" || return $?
)

harn_require_binary_freshness_receipt() (
  local bin="$1"
  local receipt=""
  local manifest=""
  local checker=""

  trap 'exit 129' HUP
  trap 'exit 130' INT
  trap 'exit 143' TERM

  receipt="$(harn_binary_freshness_receipt_path "$bin")" || return $?
  manifest="$(harn_binary_freshness_manifest_path "$bin")" || return $?
  checker="$(harn_binary_freshness_checker_path "$bin")" || return $?
  if [[ ! -r "$receipt" ]]; then
    echo "error: cannot prove worktree harn binary freshness: build receipt is missing at $receipt" >&2
    harn_print_binary_freshness_recovery
    return 1
  fi
  if [[ ! -r "$manifest" ]]; then
    echo "error: cannot prove worktree harn binary freshness: input manifest is missing at $manifest" >&2
    harn_print_binary_freshness_recovery
    return 1
  fi
  if [[ ! -x "$checker" ]]; then
    echo "error: cannot prove worktree harn binary freshness: checker is missing at $checker" >&2
    harn_print_binary_freshness_recovery
    return 1
  fi
  # One small typed process owns the unchanged hot path: it binds its own
  # signed artifact and the Harn executable to the receipt, then content-hashes
  # the versioned manifest's source inputs in bounded native batches. Platform
  # metadata owns inventory and accidental artifact replacement only; it never
  # substitutes for exact source bytes. Directory-generation ambiguity fails
  # closed instead of silently falling back to a weaker heuristic.
  if ! "$checker" verify "$receipt" "$manifest" "$bin" "$(harn_repo_root)"; then
    echo "error: cannot prove worktree harn binary freshness from the exact input manifest" >&2
    harn_print_binary_freshness_recovery
    return 1
  fi
)

# Return the compiled identity from a receipt only after the canonical checker
# has rebound that receipt to the current executable, manifest, and checkout.
# CI exports this build input to later Cargo invocations so they cannot relink
# the proven binary with a different `rerun-if-env-changed` value.
harn_verified_build_freshness_id() (
  local bin="$1"
  local receipt=""
  local identity=""
  local count=""

  harn_require_binary_freshness_receipt "$bin" || return $?
  receipt="$(harn_binary_freshness_receipt_path "$bin")" || return $?
  count="$(grep -c '^build-freshness=' "$receipt" || true)"
  identity="$(sed -n 's/^build-freshness=//p' "$receipt")"
  if [[ "$count" != "1" ]] \
    || [[ ! "$identity" =~ ^([0-9a-f]{40}|[0-9a-f]{64})$ ]]; then
    echo "error: Harn freshness receipt has a missing, duplicate, or malformed build identity" >&2
    return 1
  fi
  printf '%s\n' "$identity"
)
