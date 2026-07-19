Added an opt-in `adaptive` tool-call format: a permissive union parser that runs every text-channel lane and
recovers the tool-name-as-key JSON dialect (`{"tool": {args}}`) that the pinned grammars miss, accepting a call
only when it maps unambiguously to exactly one presented tool with schema-valid arguments. Default-off — no
catalog route resolves to it; reachable only via an explicit `tool_format` pin/request.
