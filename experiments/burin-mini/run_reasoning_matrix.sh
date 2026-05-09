#!/usr/bin/env bash

set -euo pipefail

repo_root=$(
  cd "$(dirname "$0")/../.." >/dev/null 2>&1
  pwd
)
experiment_root="$repo_root/experiments/burin-mini"
env_file="${BURIN_MINI_MATRIX_ENV_FILE:-$HOME/projects/burin-code/.env}"
timestamp="$(date -u +"%Y%m%dT%H%M%SZ")"
matrix_root="${BURIN_MINI_MATRIX_ROOT:-$experiment_root/evals/reasoning-matrix/$timestamp}"
policies="${BURIN_MINI_MATRIX_POLICIES:-off auto high}"
scale="${BURIN_MINI_MATRIX_SCALE:-medium}"
semantic_mode="${BURIN_MINI_SEMANTIC_EVAL_MODE:-heuristic}"
together_limit="${BURIN_MINI_MATRIX_TOGETHER_LIMIT:-4}"
harn_bin="${HARN_BIN:-$repo_root/target/debug/harn}"

mkdir -p "$matrix_root"

if [[ -f "$env_file" ]]; then
  set -a
  # shellcheck disable=SC1090
  source "$env_file"
  set +a
fi

if [[ ! -x "$harn_bin" ]]; then
  build_target_dir="${BURIN_MINI_CARGO_TARGET_DIR:-$(mktemp -d "${TMPDIR:-/tmp}/burin-mini-target.XXXXXX")}"
  echo "Building harn CLI once for the reasoning matrix..."
  CARGO_TARGET_DIR="$build_target_dir" cargo build --quiet --bin harn --manifest-path "$repo_root/Cargo.toml"
  harn_bin="$build_target_dir/debug/harn"
fi

slug() {
  printf '%s' "$1" | tr '/: .,' '______'
}

write_case_json() {
  local path="$1"
  local provider="$2"
  local model="$3"
  local policy="$4"
  local suite_root="$5"
  python3 - "$path" "$provider" "$model" "$policy" "$scale" "$semantic_mode" "$suite_root" <<'PY'
import json
import sys

path, provider, model, policy, scale, semantic_mode, suite_root = sys.argv[1:]
with open(path, "w", encoding="utf-8") as handle:
    json.dump(
        {
            "provider": provider,
            "model": model,
            "reasoning_policy": policy,
            "reasoning_scale": scale,
            "semantic_eval_mode": semantic_mode,
            "suite_root": suite_root,
        },
        handle,
        indent=2,
        sort_keys=True,
    )
    handle.write("\n")
PY
}

run_case() {
  local provider="$1"
  local model="$2"
  local policy="$3"
  local case_name
  local case_root
  local log_path
  local suite_root

  case_name="$(slug "$provider")-$(slug "$model")-$(slug "$policy")"
  case_root="$matrix_root/$case_name"
  log_path="$case_root/run.log"
  mkdir -p "$case_root"

  echo
  echo "=== provider=$provider model=$model reasoning_policy=$policy scale=$scale ==="
  (
    cd "$repo_root"
    BURIN_MINI_PROVIDER="$provider" \
      BURIN_MINI_REASONING_POLICY="$policy" \
      BURIN_MINI_REASONING_SCALE="$scale" \
      BURIN_MINI_SEMANTIC_EVAL_MODE="$semantic_mode" \
      HARN_BIN="$harn_bin" \
      "$experiment_root/run_live_suite.sh" "$model"
  ) | tee "$log_path"

  suite_root="$(awk -F= '/^suite_root=/{print $2}' "$log_path" | tail -1)"
  write_case_json "$case_root/case.json" "$provider" "$model" "$policy" "$suite_root"
}

split_words() {
  python3 - "$1" <<'PY'
import re
import sys

for item in re.split(r"[\s,]+", sys.argv[1].strip()):
    if item:
        print(item)
PY
}

ollama_models() {
  if [[ -n "${BURIN_MINI_MATRIX_OLLAMA_MODELS:-}" ]]; then
    split_words "$BURIN_MINI_MATRIX_OLLAMA_MODELS"
    return
  fi
  printf '%s\n' \
    "qwen3.6:35b-a3b-coding-nvfp4" \
    "gemma4:26b" \
    "gemma4-128k:latest"
}

ollama_has_model() {
  local model="$1"
  command -v ollama >/dev/null 2>&1 || return 1
  ollama list | awk 'NR > 1 {print $1}' | grep -Fxq "$model"
}

stop_other_ollama_models() {
  local active="$1"
  local model
  command -v ollama >/dev/null 2>&1 || return 0
  while IFS= read -r model; do
    [[ -z "$model" || "$model" == "$active" ]] && continue
    ollama stop "$model" >/dev/null 2>&1 || true
  done < <(ollama_models)
}

llamacpp_model() {
  if [[ -n "${BURIN_MINI_MATRIX_LLAMACPP_MODEL:-}" ]]; then
    printf '%s\n' "$BURIN_MINI_MATRIX_LLAMACPP_MODEL"
    return
  fi
  local base_url="${LLAMACPP_BASE_URL:-http://127.0.0.1:8001}"
  local tmp
  tmp="$(mktemp "${TMPDIR:-/tmp}/llamacpp-models.XXXXXX.json")"
  if ! curl -fsS "$base_url/v1/models" >"$tmp" 2>/dev/null; then
    rm -f "$tmp"
    return 0
  fi
  python3 - "$tmp" <<'PY' || true
import json
import sys

try:
    with open(sys.argv[1], "r", encoding="utf-8") as handle:
        payload = json.load(handle)
except Exception:
    raise SystemExit(0)

data = payload.get("data") if isinstance(payload, dict) else None
if isinstance(data, list) and data:
    first = data[0]
    if isinstance(first, dict) and first.get("id"):
        print(first["id"])
PY
  rm -f "$tmp"
}

together_models() {
  if [[ -n "${BURIN_MINI_MATRIX_TOGETHER_MODELS:-}" ]]; then
    split_words "$BURIN_MINI_MATRIX_TOGETHER_MODELS"
    return
  fi
  local api_key="${TOGETHER_API_KEY:-${TOGETHER_AI_API_KEY:-}}"
  [[ -n "$api_key" ]] || return 0
  local base_url="${TOGETHER_API_BASE_URL:-${TOGETHER_AI_BASE_URL:-https://api.together.xyz/v1}}"
  if [[ "$base_url" != */v1 ]]; then
    base_url="${base_url%/}/v1"
  fi
  local tmp
  tmp="$(mktemp "${TMPDIR:-/tmp}/together-models.XXXXXX.json")"
  if ! curl -fsS \
    -H "Authorization: Bearer ${api_key}" \
    "${base_url%/}/models" >"$tmp"; then
    rm -f "$tmp"
    return 0
  fi
  python3 - "$tmp" "$together_limit" <<'PY'
import json
import re
import sys

path, limit_raw = sys.argv[1:]
limit = int(limit_raw)
with open(path, "r", encoding="utf-8") as handle:
    payload = json.load(handle)

models = payload.get("data", payload) if isinstance(payload, dict) else payload
if not isinstance(models, list):
    raise SystemExit(0)

def numeric(value):
    if isinstance(value, (int, float)):
        return float(value)
    if isinstance(value, str):
        match = re.search(r"\d+(?:\.\d+)?", value)
        if match:
            return float(match.group(0))
    return None

def price_mtok(model, *keys):
    pricing = model.get("pricing") if isinstance(model, dict) else None
    if not isinstance(pricing, dict):
        return None
    for key in keys:
        value = pricing.get(key)
        parsed = numeric(value)
        if parsed is not None:
            return parsed
    return None

keywords = re.compile(r"(qwen|deepseek|kimi|moonshot|glm|zai|gemma|gpt-oss|coder)", re.I)
selected = []
for model in models:
    if not isinstance(model, dict):
        continue
    if not str(model.get("uuid") or "").startswith("endpoint-"):
        continue
    model_id = model.get("id") or model.get("name") or model.get("model")
    if not model_id or not keywords.search(model_id):
        continue
    input_price = price_mtok(model, "input", "input_per_mtok", "prompt")
    output_price = price_mtok(model, "output", "output_per_mtok", "completion")
    if input_price is None or output_price is None:
        continue
    if input_price <= 2.0 and output_price <= 2.0:
        lower_id = model_id.lower()
        priority = 50
        if "qwen3-coder-next" in lower_id:
            priority = 0
        elif "qwen3-coder" in lower_id:
            priority = 1
        elif "qwen" in lower_id and "coder" in lower_id:
            priority = 2
        elif "qwen" in lower_id:
            priority = 3
        elif "deepseek" in lower_id or "glm" in lower_id or "kimi" in lower_id:
            priority = 4
        elif "gemma" in lower_id:
            priority = 5
        elif "gpt-oss" in lower_id:
            priority = 6
        selected.append((priority, output_price, input_price, model_id))

for _, _, _, model_id in sorted(selected)[:limit]:
    print(model_id)
PY
  rm -f "$tmp"
}

echo "matrix_root=$matrix_root"
echo "policies=$policies"
echo "semantic_eval_mode=$semantic_mode"

if [[ "${BURIN_MINI_MATRIX_INCLUDE_OLLAMA:-1}" != "0" ]]; then
  while IFS= read -r model; do
    [[ -z "$model" ]] && continue
    if ! ollama_has_model "$model"; then
      echo "Skipping missing Ollama model: $model" >&2
      continue
    fi
    stop_other_ollama_models "$model"
    for policy in $policies; do
      run_case "ollama" "$model" "$policy"
    done
    ollama stop "$model" >/dev/null 2>&1 || true
  done < <(ollama_models)
fi

if [[ "${BURIN_MINI_MATRIX_INCLUDE_LLAMACPP:-1}" != "0" ]]; then
  model="$(llamacpp_model)"
  if [[ -n "$model" ]]; then
    export LLAMACPP_BASE_URL="${LLAMACPP_BASE_URL:-http://127.0.0.1:8001}"
    for policy in $policies; do
      run_case "llamacpp" "$model" "$policy"
    done
  else
    echo "Skipping llama.cpp: no OpenAI-compatible server reachable." >&2
  fi
fi

if [[ "${BURIN_MINI_MATRIX_INCLUDE_TOGETHER:-1}" != "0" ]]; then
  while IFS= read -r model; do
    [[ -z "$model" ]] && continue
    for policy in $policies; do
      run_case "together" "$model" "$policy"
    done
  done < <(together_models)
fi

echo
echo "Reasoning matrix complete."
echo "matrix_root=$matrix_root"
