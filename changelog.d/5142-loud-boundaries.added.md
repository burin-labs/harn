- Every boundary between model-produced bytes and executed action now reports
  losses through one typed funnel. `AgentEvent::BoundaryFailure` carries the
  boundary, what happened to the output (`dropped` / `unrecognized` /
  `truncated` / `killed` / `capped`), an owner in the same vocabulary as
  `AgentTerminalKind::owner`, and an excerpt of the bytes that died. Hosts
  receive it over ACP as `_harn/agentEvent` with kind `boundary_failure`, and
  `.harn` boundaries emit the same event through
  `agent_emit_event(session, "boundary_failure", {...})`.
- Six boundaries that used to lose model output silently now emit it: the text
  tool-call parse handoff (an unrecognized dialect that degraded to prose),
  provider response ingestion (content blocks, output items, message parts, and
  completions past `choices[0]` with no handler), visible-text sanitization
  (assistant prose superseded by a `<user_response>` block), the
  `__host_agent_emit_event` allowlist (rejections that every stdlib caller's
  `try { }` swallowed), the rate governor's admission gate (a call that
  proceeds unreserved after the wait cap), and `agent_chat_loop`'s turn and
  input caps.
- `make check-loud-boundaries` keeps the invariant from rotting.
  `scripts/loud_boundaries.toml` enumerates every boundary, and the gate fails
  when the `BoundaryId` enum and the registry drift apart, when a registered
  boundary stops reporting, when a file nobody registered starts reporting one,
  or when a boundary's test goes missing or inert.
