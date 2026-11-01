Managed services can report an unavailable included allowance without misclassifying it as a provider account billing
failure. The refusal stops without retrying and preserves the service's explanation and contact remedy separately
from transport diagnostics.

Migration: Rust callers constructing `AcpPromptFailureFacts` exhaustively must initialize the new `message` and
`detail` fields to `None`, or use `..AcpPromptFailureFacts::default()`. Hosts should regenerate protocol bindings
and handle the new `managed_allowance_unavailable` terminal class by showing the service message without retrying.
