#!/usr/bin/env bash
# Driver for the question-at-a-time planning agent (qmode).
#
# Subcommands:
#   chat <task_id> [<task_string>]   Interactive REPL: drives init+answer in a
#                                    loop, prompting at the terminal until a
#                                    plan lands. Resumes if task already exists.
#   init <task_id> <task_string>     Create task dir + run first turn (one-shot).
#   answer <task_id> <answer_text>   Record answer for pending question, run next turn.
#   show <task_id>                   Print pending question or final plan.
#   reset <task_id>                  Wipe the task dir.
#   inspect <task_id>                Run qmode_inspect.py on the latest run record.
#
# Exit codes:
#   0 = plan emitted (also after `show` of a plan, or `reset`)
#   10 = paused on a pending question (also after `show` of a pending)
#   non-zero = harness or validation error
#
# Env overrides:
#   QMODE_PROVIDER (default: ollama)
#   QMODE_MODEL    (default: qwen3.6:35b-a3b-coding-nvfp4)

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
EXPERIMENT_DIR="$REPO_ROOT/experiments/burin-mini"
PROVIDER="${QMODE_PROVIDER:-ollama}"
MODEL="${QMODE_MODEL:-qwen3.6:35b-a3b-coding-nvfp4}"

usage() {
  sed -n '2,18p' "${BASH_SOURCE[0]}"
  exit 64
}

[[ $# -lt 1 ]] && usage
cmd="$1"; shift

task_dir() {
  echo "$EXPERIMENT_DIR/.qmode/$1"
}

ensure_jq() {
  if ! command -v jq >/dev/null 2>&1; then
    echo "[run_qmode] ERROR: jq is required."; exit 2
  fi
}

run_pipeline() {
  local task_id="$1"
  local task_arg="${2:-}"
  local td; td="$(task_dir "$task_id")"
  mkdir -p "$td"
  local mock_path="$td/llm-mock.jsonl"
  pushd "$REPO_ROOT" >/dev/null
  set +e
  QMODE_TASK_ID="$task_id" \
  BURIN_MINI_PROVIDER="$PROVIDER" \
  BURIN_MINI_GENERATOR_MODEL="$MODEL" \
    cargo run --quiet --bin harn -- playground \
      --host "$EXPERIMENT_DIR/host.harn" \
      --script "$EXPERIMENT_DIR/pipeline_qmode.harn" \
      --task "$task_arg" \
      --llm-mock-record "$mock_path" \
      2>&1 | tee "$td/last-stdout.log"
  local rc=${PIPESTATUS[0]}
  set -e
  popd >/dev/null
  return "$rc"
}

post_run_dispatch() {
  local task_id="$1"
  local td; td="$(task_dir "$task_id")"
  local step="(missing)"
  [[ -f "$td/step.txt" ]] && step="$(cat "$td/step.txt")"
  if [[ -f "$td/plan.json" ]]; then
    echo "[run_qmode] step=${step}"
    echo "=== PLAN ==="
    jq . "$td/plan.json"
    return 0
  fi
  if [[ -f "$td/pending.json" ]]; then
    local qa_count=0
    [[ -f "$td/qa.jsonl" ]] && qa_count=$(wc -l < "$td/qa.jsonl" | tr -d ' ')
    echo "[run_qmode] step=${step}  prior_q&a=${qa_count}"
    echo "=== PENDING QUESTION ==="
    jq -r '.question' "$td/pending.json"
    return 10
  fi
  echo "[run_qmode] step=${step}"
  echo "[run_qmode] ERROR: no plan.json or pending.json after run."
  if [[ -f "$td/diag.json" ]]; then
    echo "--- diag.json ---"
    jq . "$td/diag.json"
  fi
  echo "--- last-stdout.log (tail) ---"
  tail -40 "$td/last-stdout.log" 2>/dev/null || true
  return 1
}

case "$cmd" in
  init)
    [[ $# -ge 2 ]] || usage
    ensure_jq
    task_id="$1"; shift
    task_text="$*"
    td="$(task_dir "$task_id")"
    if [[ -d "$td" ]]; then
      echo "[run_qmode] ERROR: task '$task_id' already exists. Use 'reset' first or 'answer' to continue."
      exit 1
    fi
    mkdir -p "$td"
    run_pipeline "$task_id" "$task_text" || true
    post_run_dispatch "$task_id"
    ;;
  answer)
    [[ $# -ge 2 ]] || usage
    ensure_jq
    task_id="$1"; shift
    answer_text="$*"
    td="$(task_dir "$task_id")"
    if [[ ! -f "$td/pending.json" ]]; then
      echo "[run_qmode] ERROR: no pending question for task '$task_id'."
      exit 1
    fi
    if [[ -f "$td/plan.json" ]]; then
      echo "[run_qmode] task already complete. Plan:"
      jq . "$td/plan.json"
      exit 0
    fi
    q="$(jq -r '.question' "$td/pending.json")"
    asked="$(jq -r '.asked_at' "$td/pending.json")"
    jq -nc --arg q "$q" --arg a "$answer_text" --arg ask "$asked" --arg ans "$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
      '{question:$q, answer:$a, asked_at:$ask, answered_at:$ans}' >> "$td/qa.jsonl"
    rm "$td/pending.json"
    task_text="$(jq -r '.task' "$td/task.json")"
    run_pipeline "$task_id" "$task_text" || true
    post_run_dispatch "$task_id"
    ;;
  show)
    [[ $# -ge 1 ]] || usage
    ensure_jq
    task_id="$1"
    td="$(task_dir "$task_id")"
    if [[ -f "$td/plan.json" ]]; then
      echo "=== PLAN ==="
      jq . "$td/plan.json"
      exit 0
    fi
    if [[ -f "$td/pending.json" ]]; then
      echo "=== PENDING ==="
      jq . "$td/pending.json"
      echo
      echo "=== Q&A SO FAR ==="
      [[ -f "$td/qa.jsonl" ]] && cat "$td/qa.jsonl" | jq . || echo "(none)"
      exit 10
    fi
    echo "[run_qmode] no state for '$task_id'."
    exit 1
    ;;
  reset)
    [[ $# -ge 1 ]] || usage
    task_id="$1"
    td="$(task_dir "$task_id")"
    rm -rf "$td"
    echo "[run_qmode] reset $task_id"
    ;;
  inspect)
    [[ $# -ge 1 ]] || usage
    task_id="$1"
    python3 "$EXPERIMENT_DIR/qmode_inspect.py" "$(task_dir "$task_id")"
    ;;
  chat)
    [[ $# -ge 1 ]] || usage
    ensure_jq
    task_id="$1"; shift
    td="$(task_dir "$task_id")"
    initial_task="${*:-}"
    if [[ ! -d "$td" ]]; then
      if [[ -z "$initial_task" ]]; then
        echo -n "Task description: "
        read -r initial_task
      fi
      echo "[qmode chat] starting task '$task_id'..."
      mkdir -p "$td"
      run_pipeline "$task_id" "$initial_task" >/dev/null
    elif [[ -f "$td/plan.json" ]]; then
      echo "[qmode chat] task '$task_id' already complete. Plan:"
      jq . "$td/plan.json"
      exit 0
    fi
    while true; do
      if [[ -f "$td/plan.json" ]]; then
        echo
        echo "=== PLAN ==="
        jq . "$td/plan.json"
        echo
        echo "[qmode chat] done."
        exit 0
      fi
      if [[ ! -f "$td/pending.json" ]]; then
        echo "[qmode chat] no plan and no pending question — pipeline failed. See:"
        echo "  $td/diag.json"
        echo "  $td/last-stdout.log"
        exit 1
      fi
      qa_count=0
      [[ -f "$td/qa.jsonl" ]] && qa_count=$(wc -l < "$td/qa.jsonl" | tr -d ' ')
      q="$(jq -r '.question' "$td/pending.json")"
      echo
      echo "── Q$((qa_count + 1)) ──"
      echo "$q"
      echo -n "> "
      if ! IFS= read -r answer_text; then
        echo
        echo "[qmode chat] EOF — exiting. Resume with: $0 chat $task_id"
        exit 130
      fi
      if [[ -z "$answer_text" ]]; then
        echo "[qmode chat] empty answer — re-prompting."
        continue
      fi
      asked="$(jq -r '.asked_at' "$td/pending.json")"
      jq -nc --arg q "$q" --arg a "$answer_text" --arg ask "$asked" --arg ans "$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
        '{question:$q, answer:$a, asked_at:$ask, answered_at:$ans}' >> "$td/qa.jsonl"
      rm "$td/pending.json"
      task_text="$(jq -r '.task' "$td/task.json")"
      echo "[qmode chat] thinking..."
      run_pipeline "$task_id" "$task_text" >/dev/null
    done
    ;;
  *)
    usage
    ;;
esac
