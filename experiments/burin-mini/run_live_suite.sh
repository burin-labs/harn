#!/usr/bin/env bash

set -euo pipefail

repo_root=$(
  cd "$(dirname "$0")/../.." >/dev/null 2>&1
  pwd
)
experiment_root="$repo_root/experiments/burin-mini"
model="${1:-${BURIN_MINI_OLLAMA_MODEL:-qwen3.5:35b-a3b-coding-nvfp4}}"
timestamp="$(date -u +"%Y%m%dT%H%M%SZ")"
model_slug="$(printf '%s' "$model" | tr '/: ' '___')"
suite_root="$experiment_root/evals/live/${timestamp}-${model_slug}"
sandbox_root="$(mktemp -d "${TMPDIR:-/tmp}/burin-mini-live.XXXXXX")"
harn_bin="$repo_root/target/debug/harn"

mkdir -p "$suite_root"
trap 'rm -rf "$sandbox_root"' EXIT

echo "Building harn CLI once for the live suite..."
cargo build --quiet --bin harn --manifest-path "$repo_root/Cargo.toml"

run_task() {
  local task_id="$1"
  local prompt="$2"
  local task_root="$suite_root/$task_id"
  local sandbox_task="$sandbox_root/$task_id"
  local sandbox_experiment="$sandbox_task/experiment"

  mkdir -p "$task_root" "$sandbox_task"
  cp -R "$experiment_root" "$sandbox_experiment"

  echo
  echo "=== $task_id ==="
  echo "prompt: $prompt"

  (
    cd "$repo_root"
    HARN_LLM_TRANSCRIPT_DIR="$task_root/llm" \
      HARN_EVENT_LOG_DIR="$task_root/events" \
      "$harn_bin" playground \
      --host "$sandbox_experiment/host.harn" \
      --script "$sandbox_experiment/pipeline.harn" \
      --llm "ollama:$model" \
      --task "$prompt"
  ) | tee "$task_root/output.txt"

  cp "$sandbox_experiment/evals/generated/${task_id}-latest.json" "$task_root/report.json"
  cp -R "$sandbox_experiment/workspace" "$task_root/workspace_after"
}

run_task "explain_repo" "Explain this repo to me in simple terms"
run_task "comment_file" "Comment what this file does"
run_task "rate_limit_auth" "Add rate limiting middleware to the auth module"

echo
echo "Live suite complete."
echo "suite_root=$suite_root"
