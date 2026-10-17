#!/usr/bin/env bash

# One budget owns execution, historical evidence and workflow projections.
consumer_canary_policy() {
  jq -ce '
    if keys == ["deadline_seconds", "poll_seconds", "window_seconds"] and
      all(.[]; type == "number" and . > 0 and . == floor) and
      .poll_seconds <= .window_seconds and .window_seconds < .deadline_seconds
    then . + {max_windows:(.deadline_seconds / .window_seconds | ceil)}
    else error("invalid consumer observation budget contract") end
  ' "$(cd "$(dirname "${BASH_SOURCE[0]}")/../ci" && pwd)/consumer_canary_policy.json"
}
