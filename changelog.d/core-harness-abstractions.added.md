- **Agent and workflow stdlib now include reusable harness-building blocks.**
  `std/agent/stack` centralizes provider/model option resolution, capability
  cleanup, LLM caller middleware, and tool middleware; `std/agent/stream` adds
  split-safe private-span filtering for streaming chat UIs; `std/workflow/patterns`
  adds common graph builders and typed route failover helpers.

- **Workflow retry policies now execute explicit stage attempts.**
  `retry_policy.max_attempts` now retries VM-executed workflow stage paths,
  including deterministic command verifiers, records every attempt, and stops on
  first success.
