#!/usr/bin/env bash
set -euo pipefail
root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
source "$root/scripts/lib/release_consumer_verdict.sh"
scratch=$(mktemp -d "${TMPDIR:-/tmp}/harn-observation-policy.XXXXXX")
trap 'rm -rf "$scratch"' EXIT
mkdir "$scratch/bin"
cat > "$scratch/bin/gh" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
[[ "$1" == api && "$2" == 'repos/burin-labs/harn/contents/scripts/ci/consumer_observation_budget.json?ref=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa' && "$3" == --jq ]] || exit 1
printf '%s\n' "$2" >> "$STUB/reads"
jq -er "$4" "$STUB/metadata"
SH
chmod +x "$scratch/bin/gh"
export STUB="$scratch" PATH="$scratch/bin:$PATH"
jq -n --rawfile policy "$root/scripts/ci/consumer_observation_budget.json" '{type:"file",
  path:"scripts/ci/consumer_observation_budget.json",encoding:"base64",
  size:($policy|length),content:($policy|@base64)}' > "$scratch/metadata"
head=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa
legacy='[{"environment":{"CANARY_DEADLINE_SECONDS":"7200"}}]'
current='[{"environment":{"CANARY_DEADLINE_SECONDS":"9900"}}]'
old=$(release_observation_policy "$legacy")
[[ $(jq -r .budget_version <<< "$old") == 1 ]]
[[ $(jq -r .max_windows <<< "$old") == 3 ]]
[[ ! -e "$scratch/reads" ]]
new=$(release_observation_policy "$current" burin-labs/harn "$head")
[[ $(jq -r .budget_version <<< "$new") == 2 ]]
[[ $(jq -r .max_windows <<< "$new") == 4 ]]
[[ $(jq -r .deadline_reason <<< "$new") == aggregate_deadline ]]
[[ $(wc -l < "$scratch/reads") -eq 1 ]]
if release_observation_policy "$current"; then echo 'missing source provenance accepted' >&2; exit 1; fi
if release_observation_policy '[]' burin-labs/harn "$head"; then echo 'empty observation accepted' >&2; exit 1; fi
if release_observation_policy '[{"environment":{"CANARY_DEADLINE_SECONDS":"7200"}},{"environment":{"CANARY_DEADLINE_SECONDS":"9900"}}]' burin-labs/harn "$head"; then
  echo 'changing original budget accepted' >&2; exit 1
fi
jq '.path="other.json"' "$scratch/metadata" > "$scratch/changed"
mv "$scratch/changed" "$scratch/metadata"
if release_observation_policy "$current" burin-labs/harn "$head"; then echo 'wrong policy path accepted' >&2; exit 1; fi
jq '.path="scripts/ci/consumer_observation_budget.json" | .content=("{}"|@base64)' "$scratch/metadata" > "$scratch/changed"
mv "$scratch/changed" "$scratch/metadata"
if release_observation_policy "$current" burin-labs/harn "$head"; then echo 'empty policy accepted' >&2; exit 1; fi
echo 'Historical observation policy: original v1 retained; v2 source provenance and refusal controls passed'
