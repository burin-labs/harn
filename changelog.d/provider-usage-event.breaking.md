- **Provider usage is part of the agent event stream.** Each settled provider
  attempt produces a typed `ProviderUsage` event, including preparation calls
  and unpriced attempts, in the existing ordered durable and ACP streams.

  Migration: Rust consumers matching `AgentEvent` must add a wildcard arm because
  the enum is now non-exhaustive. Consumers that account for provider costs can
  handle `AgentEvent::ProviderUsage` using its cumulative run usage ledger.
