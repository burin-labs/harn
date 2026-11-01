#!/usr/bin/env bash
# The one file digest helper for shell scripts.
#
# `sha256sum PATH` is not a digest: GNU coreutils prefixes the line with a
# backslash when PATH contains one, which every absolute Windows path does, so
# "the first field" of that line is `\<hex>` on Windows and `<hex>` elsewhere.
# Hashing standard input names no file, so the line carries no escape marker,
# and the result is refused unless it is exactly 64 lowercase hex characters.

# sha256_file_hex PATH: print the lowercase hex sha256 of PATH.
sha256_file_hex() {
  local path="${1:?sha256_file_hex requires a path}"
  local line digest
  if [[ ! -f "$path" ]]; then
    echo "error: sha256_file_hex: $path is not a file" >&2
    return 1
  fi
  if command -v sha256sum >/dev/null 2>&1; then
    line="$(sha256sum < "$path")" || return 1
  elif command -v shasum >/dev/null 2>&1; then
    line="$(shasum -a 256 < "$path")" || return 1
  else
    echo "error: sha256_file_hex: neither sha256sum nor shasum is available" >&2
    return 1
  fi
  digest="${line%% *}"
  if [[ ! "$digest" =~ ^[0-9a-f]{64}$ ]]; then
    echo "error: sha256_file_hex: $path hashed to '$line', not a 64-hex digest" >&2
    return 1
  fi
  printf '%s\n' "$digest"
}

# sha256_text_hex TEXT: print the lowercase hex sha256 of TEXT's exact bytes.
sha256_text_hex() {
  local text="${1?sha256_text_hex requires text}" line digest
  if command -v sha256sum >/dev/null 2>&1; then
    line="$(printf '%s' "$text" | sha256sum)" || return 1
  elif command -v shasum >/dev/null 2>&1; then
    line="$(printf '%s' "$text" | shasum -a 256)" || return 1
  else
    echo "error: sha256_text_hex: neither sha256sum nor shasum is available" >&2
    return 1
  fi
  digest="${line%% *}"
  [[ "$digest" =~ ^[0-9a-f]{64}$ ]] || return 1
  printf '%s\n' "$digest"
}
