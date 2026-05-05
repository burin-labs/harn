# Autonomy tiers

Harn enforces graduated autonomy at side-effect boundaries. Agent and workflow
code can choose a tier, but the VM is responsible for deciding whether a
mutating builtin actually runs.

The tiers are:

| Tier | VM behavior |
|---|---|
| `shadow` | Records the would-have side effect to the TrustGraph and returns without mutating state. |
| `suggest` | Emits a proposal and a HITL `ApprovalRequest`, records the decision to the TrustGraph, and returns without mutating state. |
| `act_with_approval` | Opens a HITL approval request before the side effect. In trigger dispatch, an unapproved request suspends the dispatch with a waitpoint; an approved request lets the side effect run. |
| `act_auto` | Runs side effects directly within the active capability policy and budget. |

Trigger bindings carry `autonomy_tier`, and `trust.promote` /
`trust.demote` TrustGraph records can change the effective tier over time.
For non-trigger orchestration, scope policy with `with_autonomy_policy`:

```harn
import { autonomy_policy } from "std/agent/options"

with_autonomy_policy(
  autonomy_policy("suggest", {agent: "release-captain", reviewers: ["maintainer"]}),
  fn() {
    write_file("release-notes.md", "candidate")
  },
)
```

Per-action-class overrides use builtin names such as `write_file` or classes
such as `fs.write`:

```harn
with_autonomy_policy(
  {
    agent_id: "docs-bot",
    autonomy_tier: "act_auto",
    action_tiers: {"fs.write": "act_with_approval"},
    reviewers: ["maintainer"],
  },
  fn() {
    write_file("docs/src/index.md", "# Docs")
  },
)
```

Autonomy enforcement records use TrustGraph action classes like `fs.write`,
`process.exec`, and `git.write`, with metadata that includes the builtin name,
arguments, and approval request id when one exists.
