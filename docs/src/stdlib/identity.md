# Identity stdlib

`std/identity` provides Harn-native helpers for inspecting RFC 8693-style
`ActorChain` dicts. Use it when a script, connector, status surface, or audit
receipt needs the same current-actor-through-origin view that Harn's Rust
`ActorChain` type uses internally.

```harn
import { actor_chain_format, actor_chain_report } from "std/identity"

pipeline default() {
  const chain = {
    sub: "user:owner",
    scopes: ["repo:read", "repo:write"],
    may_act: {sub: "agent:delegate"},
    act: {
      sub: "agent:reviewer",
      scopes: ["repo:read"],
      act: {sub: "agent:planner", scope: "repo:read"},
    },
  }

  const report = actor_chain_report(chain)
  const display = actor_chain_format(chain)
}
```

## Report Shape

`actor_chain_report(chain)` never throws. It returns:

| Field | Description |
|---|---|
| `ok` | `true` when the chain is structurally valid |
| `issues` | Structured `{code, path, message}` validation issues |
| `subjects` | Current actor first, origin last |
| `current` | The current actor, or the origin for non-delegated chains |
| `origin` | The top-level `sub` principal |
| `actors` | Actor entries excluding the origin |
| `prior_actors` | Actors after `current` |
| `depth` | Number of subjects in the chain |
| `delegated` | `true` when the chain has one or more acting agents |
| `may_act` | Optional authorized-actor hint from `may_act.sub` |

Each subject includes `subject`, `kind`, `id`, `role`, `position`, `path`, and
`scopes`. `scope` strings and `scopes` lists are normalized into a deduplicated
`scopes` list.

## Throwing Helpers

Use `actor_chain_require(chain)` when malformed input should fail fast. The
other convenience helpers call it:

| Function | Use |
|---|---|
| `actor_chain_subjects(chain)` | Return current-first subject entries |
| `actor_chain_summary(chain)` | Return only the stable summary fields |
| `actor_chain_format(chain, options?)` | Render compact status text |
| `actor_chain_subject_parts(subject)` | Split `kind:id` subjects |

`actor_chain_format(chain)` renders `current for origin` and includes
`via prior` actors when present. Pass `{style: "arrow"}` to render the full
current-to-origin chain with ` -> ` separators.
