#!/usr/bin/env bash
# Project workspace-resolved features onto a narrower set of test packages.
# --workspace-except names every excluded member instead of every selected one.
set -euo pipefail
(( $# > 0 )) || { echo 'workspace features: at least one package is required' >&2; exit 2; }
cargo metadata --locked --format-version 1 | jq -er --args '
  . as $metadata
  | [$metadata.packages[]
      | select(.source == null)
      | select(.id as $id | $metadata.workspace_members | index($id))
      | .name] as $members
  | (if $ARGS.positional[0] == "--workspace-except" then
       $ARGS.positional[1:] as $excluded
       | if ($excluded | length) == 0
            or ($excluded | unique | length) != ($excluded | length)
            or (($excluded - $members) | length) != 0
         then error("invalid workspace exclusions") else $members - $excluded end
     else $ARGS.positional end) as $names
  | if ($names | length) == 0 then error("no selected workspace packages") else . end
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
' -- "$@"
