#!/bin/sh

# The one owner of how Harn runs rustc's front end, for local setup and CI.
#
# rustc type-checks a crate on one thread unless `-Zthreads` asks for more.
# harn-vm is the build's long pole, and eight front-end threads cut its own
# compile from 58s to 32s and a harn rebuild after a harn-vm edit from 90s to
# 55s. The flag is unstable, so RUSTC_BOOTSTRAP=1 lets the pinned stable
# compiler accept it. The value is a constant rather than a per-machine core
# count because rustc flags are part of every Cargo, sccache, and target-seed
# key, and one value lets every runner pool share them. rustc's front-end
# threads draw from Cargo's jobserver, so a smaller machine does not
# oversubscribe.
#
# HARN_RUSTC_FRONTEND_THREADS=1 turns it off.

harn_rustc_frontend_threads() {
  threads="${HARN_RUSTC_FRONTEND_THREADS:-8}"
  case "$threads" in
    '' | *[!0-9]* | 0)
      echo "error: HARN_RUSTC_FRONTEND_THREADS must be a positive integer" >&2
      return 2
      ;;
  esac
  printf '%s\n' "$threads"
}

# The rustc flags to add, empty when the front end stays serial.
harn_rustc_frontend_flags() {
  threads="$(harn_rustc_frontend_threads)" || return $?
  if [ "$threads" -gt 1 ]; then
    printf '%s\n' "-Zthreads=$threads"
  fi
}
