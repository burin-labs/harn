#!/usr/bin/env bash
set -euo pipefail
root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
scratch=$(mktemp -d)
trap 'rm -rf "$scratch"' EXIT
mkdir "$scratch/bin"
cd "$root"
node -e 'const fs=require("node:fs"); const yaml=require("js-yaml"); const workflow=yaml.load(fs.readFileSync(".github/workflows/rust-cache-refresh.yml","utf8")); const step=workflow.jobs["rust-cache-refresh"].steps.find(s=>s.name==="Prepare Linux cache generation save"); if(!step || typeof step.run!=="string") throw Error("owning admission step missing"); process.stdout.write(step.run);' > "$scratch/admit.sh"
cat > "$scratch/bin/gh" <<'GH'
#!/usr/bin/env bash
set -euo pipefail
printf '%s\n' "$*" >> "$PROBE_LOG"
case "$*" in
  'api --paginate repos/burin-labs/harn/actions/caches?ref=refs/heads/main&per_page=100 --slurp')
    printf '%s\n' '[{"total_count":1,"actions_caches":[{"id":123,"ref":"refs/heads/main","key":"v0-rust-workspace-tests-current","size_in_bytes":1073741824}]}]'
    ;;
  'api --paginate repos/burin-labs/harn/actions/caches?per_page=100 --slurp')
    if [[ "$PROBE_MODE" == api-failure ]]; then echo 'probe cache inventory unavailable' >&2; exit 23; fi
    case "$PROBE_MODE" in
      partial) printf '%s\n' '[{"total_count":2,"actions_caches":[]}]'; exit 0 ;;
      empty) printf '%s\n' '[]'; exit 0 ;;
      unreported) printf '%s\n' '[{"actions_caches":[]}]'; exit 0 ;;
      malformed) printf '%s\n' '[{"total_count":1,"actions_caches":[{"id":123,"key":"resident","ref":"refs/heads/main"}]}]'; exit 0 ;;
    esac
    bytes=9663676416
    if [[ "$PROBE_MODE" == fits ]]; then bytes=1073741824; fi
    if [[ -e "$PROBE_LOG.deleted" ]]; then
      printf '[{"total_count":1,"actions_caches":[{"id":456,"ref":"refs/heads/main","key":"v0-rust-harn-ci-cli-current","size_in_bytes":%s}]}]\n' "$bytes"
    else
      printf '[{"total_count":2,"actions_caches":[{"id":456,"ref":"refs/heads/main","key":"v0-rust-harn-ci-cli-current","size_in_bytes":%s},{"id":123,"ref":"refs/heads/main","key":"v0-rust-workspace-tests-current","size_in_bytes":1073741824}]}]\n' "$bytes"
    fi
    ;;
  'cache delete 123 --repo burin-labs/harn') touch "$PROBE_LOG.deleted" ;;
  *) echo "unexpected gh call: $*" >&2; exit 64 ;;
esac
GH
chmod +x "$scratch/bin/gh"
export PATH="$scratch/bin:$PATH" GITHUB_REPOSITORY=burin-labs/harn
export HARN_CACHE_FAMILY_PREFIX=v0-rust-workspace-tests- HARN_CACHE_SAVE_HEADROOM_BYTES=4294967296
for mode in insufficient api-failure partial empty unreported malformed fits; do
  export PROBE_MODE="$mode" PROBE_LOG="$scratch/$mode.calls"
  status=0
  bash -e "$scratch/admit.sh" > "$scratch/$mode.out" 2> "$scratch/$mode.err" || status=$?
  if [[ "$mode" == fits ]]; then
    [[ "$status" == 0 ]]
    jq -e '.mode == "ensure_headroom" and .deficit_bytes == 0 and .deleted == []' "$scratch/$mode.out" > /dev/null
  else
    [[ "$status" != 0 ]]
    if [[ "$mode" == insufficient ]]; then
      grep -Fq 'without deleting protected CI caches' "$scratch/$mode.err"
      jq -e '.observed == 2 and .pending == 0 and .bad == 0 and .deficit_bytes > 0 and (.protected_generations | length) == 2' "$scratch/$mode.out" > /dev/null
    elif [[ "$mode" == api-failure ]]; then
      grep -Fq 'probe cache inventory unavailable' "$scratch/$mode.err"
    else
      grep -Fq 'Cache inventory' "$scratch/$mode.err"
    fi
  fi
  grep -Fq 'actions/caches?per_page=100' "$PROBE_LOG"
  if grep -q '^cache delete ' "$PROBE_LOG"; then
    echo "admission $mode deleted the sole cache before a replacement was uploaded" >&2
    cat "$PROBE_LOG" >&2
    exit 1
  fi
  echo "admission $mode preserved the sole cache"
done
