#!/usr/bin/env bash

set -euo pipefail

repo_root=$(
  cd "$(dirname "$0")/../.." >/dev/null 2>&1
  pwd
)
experiment_root="$repo_root/experiments/burin-mini"
provider="${BURIN_MINI_PROVIDER:-ollama}"
model="${1:-${BURIN_MINI_OLLAMA_MODEL:-qwen3.5:35b-a3b-coding-nvfp4}}"
timestamp="$(date -u +"%Y%m%dT%H%M%SZ")"
model_slug="$(printf '%s' "$model" | tr '/: ' '___')"
suite_root="$experiment_root/evals/live/${timestamp}-${model_slug}"
sandbox_root="$(mktemp -d "${TMPDIR:-/tmp}/burin-mini-live.XXXXXX")"
harn_bin="${HARN_BIN:-$repo_root/target/debug/harn}"
semantic_provider="${BURIN_MINI_SEMANTIC_EVAL_PROVIDER:-$provider}"
semantic_model="${BURIN_MINI_SEMANTIC_EVAL_MODEL:-${BURIN_MINI_EVALUATOR_MODEL:-$model}}"

mkdir -p "$suite_root"
trap 'rm -rf "$sandbox_root"' EXIT

if [[ ! -x "$harn_bin" ]]; then
  build_target_dir="${BURIN_MINI_CARGO_TARGET_DIR:-$(mktemp -d "${TMPDIR:-/tmp}/burin-mini-target.XXXXXX")}"
  echo "Building harn CLI once for the live suite..."
  CARGO_TARGET_DIR="$build_target_dir" cargo build --quiet --bin harn --manifest-path "$repo_root/Cargo.toml"
  harn_bin="$build_target_dir/debug/harn"
fi

run_task() {
  local task_id="$1"
  local prompt="$2"
  local task_root="$suite_root/$task_id"
  local sandbox_task="$sandbox_root/$task_id"
  local sandbox_experiment="$sandbox_task/experiment"
  local generated_root=""
  local report_source=""
  local workspace_source=""
  local run_path=""

  mkdir -p "$task_root" "$sandbox_task"
  cp -R "$experiment_root" "$sandbox_experiment"

  echo
  echo "=== $task_id ==="
  echo "prompt: $prompt"

  (
    cd "$repo_root"
    BURIN_MINI_PROVIDER="$provider" \
      BURIN_MINI_GENERATOR_MODEL="$model" \
      BURIN_MINI_EVALUATOR_MODEL="${BURIN_MINI_EVALUATOR_MODEL:-$model}" \
      HARN_LLM_TRANSCRIPT_DIR="$task_root/llm" \
      HARN_EVENT_LOG_DIR="$task_root/events" \
      "$harn_bin" playground \
      --host "$sandbox_experiment/host.harn" \
      --script "$sandbox_experiment/pipeline.harn" \
      --llm "$provider:$model" \
      --task "$prompt"
  ) | tee "$task_root/output.txt"

  report_source="$(
    find "$sandbox_experiment" -path "*/evals/generated/${task_id}-latest.json" -print -quit
  )"
  if [[ -z "$report_source" || ! -f "$report_source" ]]; then
    echo "Could not locate generated report for $task_id under $sandbox_experiment" >&2
    return 1
  fi
  generated_root="$(dirname "$report_source")"
  cp "$report_source" "$task_root/report.json"
  run_path="$(
    python3 - "$task_root/report.json" <<'PY'
import json
import sys

with open(sys.argv[1], "r", encoding="utf-8") as handle:
    print(json.load(handle).get("run_path", ""))
PY
  )"
  if [[ -n "$run_path" && -f "$run_path" ]]; then
    cp "$run_path" "$task_root/run_record.json"
  elif [[ -n "$generated_root" && -f "$generated_root/${task_id}-run.json" ]]; then
    cp "$generated_root/${task_id}-run.json" "$task_root/run_record.json"
  fi
  (
    cd "$repo_root"
    BURIN_MINI_PROVIDER="$provider" \
      BURIN_MINI_GENERATOR_MODEL="$model" \
      BURIN_MINI_EVALUATOR_MODEL="${BURIN_MINI_EVALUATOR_MODEL:-$model}" \
      BURIN_MINI_SEMANTIC_EVAL_PROVIDER="$semantic_provider" \
      BURIN_MINI_SEMANTIC_EVAL_MODEL="$semantic_model" \
      "$harn_bin" run "$experiment_root/evaluator.harn" -- \
      "$task_root/report.json" \
      "$task_root/semantic_eval.json" \
      "$task_root"
  ) | tee "$task_root/semantic_eval_output.txt"
  workspace_source="$(
    find "$sandbox_experiment" -type d -name workspace -print -quit
  )"
  if [[ -n "$workspace_source" && -d "$workspace_source" ]]; then
    cp -R "$workspace_source" "$task_root/workspace_after"
  fi
}

run_task "explain_repo" "Explain this repo to me in simple terms"
run_task "comment_file" "Comment what this file does"
run_task "rate_limit_auth" "Add rate limiting middleware to the auth module"

echo
echo "Live suite complete."
echo "suite_root=$suite_root"
