- Agent tool dispatch takes a fast path when no policy/permission machinery is
  configured: the session policy guard, execution-policy enforcement, and the
  dynamic-permission check are skipped (each is a provable no-op without a
  configured policy, permission scope, or cached session grant), and the JSON
  form of tool arguments is no longer deep-cloned twice per call.
  `perf/vm/agent_tool_dispatch` improves from ~72.5ms to ~65.5ms per run
  (3,000 dispatches; ~10% faster, ~2.3us/dispatch of avoided policy/permission
  setup; settled-min A/B on the same machine, ~1.5ms run-to-run noise). Any
  configured policy, approval, command policy, permissions option, ambient
  policy scope, or session grant routes through the unchanged slow path.
