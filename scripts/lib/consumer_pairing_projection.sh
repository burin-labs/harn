#!/usr/bin/env bash
# Transport fresh read projections to the existing containment policy, without credentials.
set -euo pipefail
source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/consumer_pairing_superset.sh"
projection=$(cat)
jq -e '.schema == "harn.consumer_pairing_projection.v1" and (.records | type == "object")' \
  <<< "$projection" >/dev/null
repository=$(jq -er .repository <<< "$projection")
paired=$(jq -er .paired <<< "$projection")
rehearsed=$(jq -er .rehearsed <<< "$projection")
superseded=$(jq -er .superseded <<< "$projection")
projection_read() {
  local destination=$1 selected_repository=$2 endpoint=$3 query=$4 value
  [[ "$selected_repository" == "$repository" ]] || return 1
  value=$(jq -er --arg endpoint "$endpoint" '.records[$endpoint]' <<< "$projection" | jq -er "$query") || return 1
  printf -v "$destination" '%s' "$value"
}
if consumer_pairing_superset_check "$repository" "$paired" "$rehearsed" "$superseded" projection_read; then
  printf '%s\n' '{"schema":"harn.consumer_pairing_check.v1","ok":true}'
else
  jq -nc --arg reason "$CONSUMER_SUPERSET_REASON" --arg detail "$CONSUMER_SUPERSET_DETAIL" \
    '{schema:"harn.consumer_pairing_check.v1",ok:false,reason:$reason,detail:$detail}'
  exit 1
fi
