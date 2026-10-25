#!/usr/bin/env bash

# Cargo's workspace rustc wrapper: compiles Harn's own crates with rustc's
# parallel front end. Cargo calls it as `<this script> <rustc> <args...>`, and
# only for workspace members, so dependencies and their build-script probes
# never see RUSTC_BOOTSTRAP. That matters: anyhow, thiserror, and proc-macro2
# switch to nightly-only code when RUSTC_BOOTSTRAP reaches their probes, which
# grew a debug harn binary by 26%. Why the flag pays off lives in
# scripts/lib/rustc_frontend.sh.
#
# This runs under bash, not sh: Linux's dash drops environment variables whose
# names are not shell identifiers, such as CARGO_BIN_EXE_harn-test-echo-env,
# before rustc can read them with env!.

case "$1" in
  -*) rustc=rustc ;;
  *)
    rustc="$1"
    shift
    ;;
esac

# shellcheck source=scripts/lib/rustc_frontend.sh
. "$(dirname "$0")/lib/rustc_frontend.sh"
flags="$(harn_rustc_frontend_flags)" || exit $?

# Only a compilation takes the flag; `rustc -vV` and `--print` probes stay as
# they are so wrappers like sccache identify the plain compiler.
case " $* " in
  *" --crate-name "*)
    if [ -n "$flags" ]; then
      RUSTC_BOOTSTRAP=1 exec "$rustc" "$@" "$flags"
    fi
    ;;
esac
exec "$rustc" "$@"
