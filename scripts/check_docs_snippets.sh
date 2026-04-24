#!/usr/bin/env bash
#
# Verify every ```harn fenced code block in docs/src/*.md parses cleanly
# via `harn check`. Used to enforce "copy/paste runnable" snippets: readers
# should be able to lift any block verbatim into a .harn file and have it
# at least type-check.
#
# Blocks tagged ```harn,ignore are skipped — use that for intentional
# partial fragments (grammar demos in language-spec.md, etc.).
#
# Exits non-zero on the first broken snippet, with the file and a numeric
# block index to make it locatable by scrolling the source.

set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"

HARN_BIN="${HARN_BIN:-}"
if [[ -z "$HARN_BIN" ]]; then
  # Resolve the Cargo target directory so this works under
  # CARGO_TARGET_DIR overrides and sccache/worktree-redirected targets
  # where `target/` in the CWD does not exist.
  target_dir=""
  if command -v cargo >/dev/null 2>&1; then
    target_dir="$(cargo metadata --no-deps --format-version 1 2>/dev/null \
      | python3 -c 'import json,sys; print(json.load(sys.stdin).get("target_directory", ""))' 2>/dev/null)"
  fi
  if [[ -z "$target_dir" ]]; then
    target_dir="${CARGO_TARGET_DIR:-target}"
  fi

  # Prefer a pre-built debug binary so the script is fast in a loop; fall
  # back to `cargo build` for fresh clones.
  if [[ -x "$target_dir/debug/harn" ]]; then
    HARN_BIN="$target_dir/debug/harn"
  else
    echo "building harn-cli (set HARN_BIN to skip)..." >&2
    cargo build -q -p harn-cli
    HARN_BIN="$target_dir/debug/harn"
  fi
fi

TMP_DIR="$(mktemp -d)"
trap 'rm -rf "$TMP_DIR"' EXIT

failures=0
checked=0
skipped=0

# Extract ```harn ... ``` blocks. We do a tiny per-file state machine in
# awk because the set of valid info strings is small and we want exact
# matching on `harn` vs `harn,ignore`.
extract_blocks() {
  local src="$1"
  local prefix="$2" # temp file prefix
  awk -v prefix="$prefix" '
    BEGIN { in_block = 0; idx = 0; skip = 0; open_line = 0 }
    /^```harn$/ {
      if (in_block == 1) {
        # Nested or unclosed fence — markdown is malformed. Surface the
        # line where the previous fence opened so the author can find it.
        print "UNCLOSED " open_line
      }
      in_block = 1
      skip = 0
      idx++
      open_line = NR
      out = prefix "_" idx ".harn"
      next
    }
    /^```harn,ignore$/ {
      if (in_block == 1) {
        print "UNCLOSED " open_line
      }
      in_block = 1
      skip = 1
      idx++
      open_line = NR
      print "SKIP " idx
      next
    }
    /^```/ && in_block == 1 {
      in_block = 0
      if (skip == 0) print "BLOCK " idx " " out
      next
    }
    in_block == 1 && skip == 0 {
      print > out
    }
    END {
      if (in_block == 1) {
        print "UNCLOSED " open_line
      }
    }
  ' "$src"
}

while IFS= read -r -d '' md_file; do
  file_prefix="$TMP_DIR/$(basename "${md_file%.md}")"
  results=$(extract_blocks "$md_file" "$file_prefix")

  while IFS= read -r line; do
    [[ -z "$line" ]] && continue
    case "$line" in
      UNCLOSED\ *)
        failures=$((failures + 1))
        open_line="${line#UNCLOSED }"
        echo
        echo "FAIL: $md_file (unclosed \`\`\`harn fence opened at line $open_line)"
        echo "      hint: the next fenced block opens before this one closes."
        ;;
      SKIP\ *)
        skipped=$((skipped + 1))
        ;;
      BLOCK\ *)
        # BLOCK <idx> <path>
        idx="${line#BLOCK }"
        idx_num="${idx%% *}"
        block_path="${idx#* }"
        checked=$((checked + 1))
        if ! "$HARN_BIN" check "$block_path" >"$block_path.out" 2>&1; then
          failures=$((failures + 1))
          echo
          echo "FAIL: $md_file (harn block #$idx_num)"
          echo "      temp file: $block_path"
          sed 's/^/      | /' "$block_path.out"
        fi
        ;;
    esac
  done <<< "$results"
done < <(find docs/src -name '*.md' -print0)

echo
echo "docs snippets: $checked checked, $skipped skipped (harn,ignore), $failures failed"
if (( failures > 0 )); then
  echo
  echo "hint: snippets must parse under \`harn check\`. Wrap bare statements"
  echo "      in \`pipeline default() { ... }\`, inline any referenced helpers,"
  echo "      or tag the block as \`\`\`harn,ignore if it's an intentional fragment."
  exit 1
fi
