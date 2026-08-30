#!/usr/bin/env bash
set -euo pipefail

usage() {
  echo "usage: $0 resolve <flag> <event> <ref> | verify <run-slow-ci> <name=result>..." >&2
  exit 64
}

case "${1:-}" in
  resolve)
    [ "$#" -eq 4 ] || usage
    flag=$2
    event=$3
    ref=$4

    # Only the exact, reversible repository-variable value enables the sprint
    # path. Missing, misspelled, or differently-cased values fail closed to the
    # full graph. Main retains the post-merge backstop while the switch is on.
    if [ "$flag" = "true" ] \
      && { [ "$event" != "push" ] || [ "$ref" != "refs/heads/main" ]; }; then
      echo false
    else
      echo true
    fi
    ;;
  verify)
    [ "$#" -ge 3 ] || usage
    run_slow_ci=$2
    shift 2
    expected_names=(
      "Linux sandbox tests"
      "Harn proof workers"
      "Windows cross-compile check"
      "Rust on macOS"
    )
    if [ "$#" -ne "${#expected_names[@]}" ]; then
      echo "error: expected ${#expected_names[@]} slow-check readings, got $#" >&2
      exit 1
    fi
    case "$run_slow_ci" in
      true | false) ;;
      *)
        echo "error: run_slow_ci did not report true or false" >&2
        exit 1
        ;;
    esac

    pending=()
    failing=()
    reading_index=0
    for reading in "$@"; do
      name=${reading%%=*}
      result=${reading#*=}
      if [ "$name" = "$reading" ] || [ -z "$name" ]; then
        echo "error: malformed slow-check reading" >&2
        exit 1
      fi
      if [ "$name" != "${expected_names[$reading_index]}" ]; then
        echo "error: slow-check reading $((reading_index + 1)) must name ${expected_names[$reading_index]}" >&2
        exit 1
      fi
      reading_index=$((reading_index + 1))
      case "$result" in
        success | skipped) ;;
        queued | in_progress | pending | requested | waiting | "") pending+=("$name") ;;
        *) failing+=("$name=$result") ;;
      esac
    done

    echo "Sprint slow checks: run_slow_ci=$run_slow_ci pending=${#pending[@]} failing=${#failing[@]}"
    if [ "${#pending[@]}" -gt 0 ]; then
      printf 'Pending slow checks: %s\n' "${pending[*]}"
    fi
    if [ "${#failing[@]}" -gt 0 ]; then
      printf 'Failing slow checks: %s\n' "${failing[*]}"
    fi

    # Deferred jobs must have either skipped or succeeded. A cancelled or
    # failed result remains red even though the slow graph is outside the
    # sprint critical path. Full-mode job-specific policy stays in ci.yml.
    if [ "$run_slow_ci" = "false" ] \
      && { [ "${#pending[@]}" -gt 0 ] || [ "${#failing[@]}" -gt 0 ]; }; then
      exit 1
    fi
    ;;
  *) usage ;;
esac
