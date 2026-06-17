# Disclosure stdlib

`std/disclosure` renders authorship disclosure artifacts from an
RFC 8693-style `ActorChain` value. Use it when a host or connector needs the
same actor chain represented in a surface-native form such as Git trailers,
a Slack byline, or a GitHub author-mode decision.

```harn
import { render } from "std/disclosure"

pipeline default() {
  let chain = {
    sub: "user:kenneth",
    act: {sub: "agent:merge-captain", act: {sub: "agent:burin"}},
  }

  let trailers = render(chain, "git")
  let byline = render(chain, "slack")
  let github = render(chain, "github")
}
```

Built-in surfaces:

| Surface | Return value |
|---|---|
| `git` | Trailer block string |
| `slack` | Byline string |
| `github` | `{kind: "github_author_choice", mode, author, co_author, principals, actor_chain}` |

## Configuration

Disclosure config is layered in this order:

1. Built-in defaults.
2. `[disclosure]` in the project `harn.toml`.
3. `HARN_DISCLOSURE_CONFIG_PATH`, then `HARN_DISCLOSURE_CONFIG`.
4. `options.config` passed to `render`.

Each overlay is TOML-shaped data. Later layers recursively override earlier
layers. Project manifests read only the nested `[disclosure]` table; standalone
environment/config files may use either `[disclosure.*]` tables or direct
`[surfaces.*]` / `[identities.*]` tables.

```toml
[disclosure.defaults]
email_domain = "example.invalid"

[disclosure.identities."user:kenneth"]
name = "Kenneth Sinder"
email = "kenneth@example.com"
github = "kennethsinder"

[disclosure.identities."agent:merge-captain"]
name = "Merge Captain"
email = "merge-captain@bots.example"
github = "merge-captain-bot"

[disclosure.surfaces.slack]
kind = "text"
template = "AI-assisted by {{ current.label }} for {{ origin.label }}."
```

Text surfaces use the regular Harn prompt-template renderer with these
bindings:

| Binding | Description |
|---|---|
| `current` | Current actor principal, or the origin when the chain has no actors |
| `origin` | Top-level `sub` principal |
| `actors` | Actor principals, current first, excluding `origin` |
| `prior_actors` | Actor principals after `current` |
| `principals` | All principals, current actor through origin |
| `subjects` | Raw subject strings in the same order as `principals` |
| `delegated` | `true` when the chain has one or more acting agents |

`HARN_DISCLOSURE_CONFIG` may contain inline TOML or a path to a TOML file.
Pass `{project: false}` or `{env: false}` to disable those layers for a
specific render call.
