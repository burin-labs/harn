- With `purpose_labels` enabled, a tool call now carries the model's declared purpose for its turn as
  `intent`. Hosts read it as `_meta.harn.intent` on the ACP `tool_call` update and as
  `toolCall._meta.harn.intent` on `session/request_permission`, so an approval prompt can say what the
  model is doing ("Looking for PR 456 artifacts") instead of only the tool name. Whitespace is collapsed,
  the value is capped at 200 characters, and the key is absent when the turn declared no purpose.
