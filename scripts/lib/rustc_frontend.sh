#!/usr/bin/env bash

# The one owner of how Harn runs rustc's front end, for local setup and CI.
#
# rustc type-checks a crate on one thread unless `-Zthreads` asks for more.
# harn-vm is the build's long pole, and eight front-end threads cut its own
# compile from 58s to 32s and a harn rebuild after a harn-vm edit from 90s to
# 55s. The flag is unstable, so scripts/rustc_frontend_wrapper.sh, Cargo's
# workspace rustc wrapper in local setup and CI, passes it with
# RUSTC_BOOTSTRAP=1 to Harn's own crates only. Dependencies compile exactly as
# on stable. The value is a constant rather than a per-machine core count so
# most machines compile the same way. The 3-core, 7-GiB hosted Mac stays serial
# because its large test compilation swaps under the parallel front end.
#
# HARN_RUSTC_FRONTEND_THREADS=1 turns it off.

harn_rustc_frontend_threads() {
  threads="${HARN_RUSTC_FRONTEND_THREADS:-}"
  if [ -z "$threads" ]; then
    threads=8
    if [ "${RUNNER_ENVIRONMENT:-}" = github-hosted ] && [ "$(uname -s)" = Darwin ]; then
      cores="$(sysctl -n hw.logicalcpu 2>/dev/null)" || cores=""
      memory="$(sysctl -n hw.memsize 2>/dev/null)" || memory=""
      case "$cores:$memory" in
        *[!0-9:]* | :* | *: | 0:* | *:0)
          echo "error: cannot measure hosted macOS compiler resources" >&2
          return 2
          ;;
      esac
      if [ "$cores:$memory" = 3:7516192768 ]; then
        threads=1
      fi
    fi
  fi
  case "$threads" in
    '' | *[!0-9]* | 0)
      echo "error: HARN_RUSTC_FRONTEND_THREADS must be a positive integer" >&2
      return 2
      ;;
  esac
  printf '%s\n' "$threads"
}

# The rustc flags to add, empty when the front end stays serial. Native
# Windows Cargo cannot launch the shell wrapper, so Windows stays serial.
harn_rustc_frontend_flags() {
  threads="$(harn_rustc_frontend_threads)" || return $?
  if [ "${OS:-}" = "Windows_NT" ]; then
    return 0
  fi
  case "$(uname -s)" in
    MINGW* | MSYS* | CYGWIN*) return 0 ;;
  esac
  if [ "$threads" -gt 1 ]; then
    printf '%s\n' "-Zthreads=$threads"
  fi
}

# Copy the workspace wrapper and this file under CACHE_ROOT, keyed by their
# content, and print the copied wrapper's path. Cargo config and CI point at
# the copy, so a checkout switched to a commit without the wrapper still
# builds. Each file lands by atomic rename, so concurrent setups are safe.
harn_rustc_frontend_install_wrapper() {
  local scripts_dir="$1" cache_root="$2" key dest file
  # cksum is POSIX, so this needs no git or sha tool; the key only names a
  # cache directory.
  key="$(cat "$scripts_dir/rustc_frontend_wrapper.sh" "$scripts_dir/lib/rustc_frontend.sh" \
    | cksum | awk '{ print $1 "-" $2 }')"
  [ -n "$key" ] || return 1
  dest="$cache_root/$key"
  mkdir -p "$dest/lib" || return 1
  for file in rustc_frontend_wrapper.sh lib/rustc_frontend.sh; do
    if [ ! -f "$dest/$file" ]; then
      cp "$scripts_dir/$file" "$dest/$file.tmp.$$" \
        && chmod +x "$dest/$file.tmp.$$" \
        && mv -f "$dest/$file.tmp.$$" "$dest/$file" || return 1
    fi
  done
  printf '%s\n' "$dest/rustc_frontend_wrapper.sh"
}
# A/B measurement branch: default 8 thread(s).
