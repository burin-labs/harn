- Agent loops now fail-fast later same-resource mutating tool calls in the same
  assistant response after an earlier sibling fails, using tool annotations and
  path arguments instead of host-specific heuristics.
