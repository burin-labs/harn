Rust hosts can capture a remembered Allow or Deny with
`ToolApprovalRequest::capture_decision`. Harn binds the returned rule to the
whole argument object, canonical workspace and current tool facts. Session and
durable stores use that same rule; changing another resource, argument or write
environment mode requires a new decision. Persisted rules contain an opaque
fingerprint rather than raw arguments that may hold credentials.
Host capture and VM execution use the same path projection, including
conventional unannotated path fields and command-reader paths, so an unchanged
invocation retains its decision without weakening workspace refusal.

Migration: Rust consumers constructing `PolicyRuleMatch` with an exhaustive
struct literal must add `invocation_sha256: None` for authored rules. Hosts
remembering a user decision should use `ToolApprovalRequest::capture_decision`
and preserve its returned matcher instead of reconstructing a partial scope.
