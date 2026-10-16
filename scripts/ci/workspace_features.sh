#!/usr/bin/env bash
# Project workspace-resolved features onto a narrower set of test packages.
set -euo pipefail
(( $# > 0 )) || { echo 'workspace features: at least one package is required' >&2; exit 2; }
cargo metadata --locked --format-version 1 | jq -er --args '
  . as $metadata
  | $ARGS.positional as $names
  | if ($names | unique | length) != ($names | length) then error("duplicate requested package") else . end
  | [$names[] as $name
     | [$metadata.packages[]
        | select(.name == $name and .source == null)
        | select(.id as $id | $metadata.workspace_members | index($id))] as $packages
     | if ($packages | length) != 1 then error("missing or ambiguous workspace package: " + $name) else . end
     | [$metadata.resolve.nodes[] | select(.id == $packages[0].id)] as $nodes
     | if ($nodes | length) != 1 or ($nodes[0].features | type) != "array"
          or any($nodes[0].features[]; type != "string" or length == 0)
       then error("unmeasured resolved features: " + $name) else . end
     | $nodes[0].features[] | $name + "/" + .]
  | sort | unique | join(",")
' "$@"
