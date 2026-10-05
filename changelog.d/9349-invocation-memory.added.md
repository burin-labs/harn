Rust hosts can capture a remembered Allow or Deny with
`ToolApprovalRequest::capture_decision`. Harn binds the returned rule to the
whole argument object, canonical workspace and current tool facts. Session and
durable stores use that same rule; changing another resource, argument or write
environment mode requires a new decision. Persisted rules contain an opaque
fingerprint rather than raw arguments that may hold credentials.
