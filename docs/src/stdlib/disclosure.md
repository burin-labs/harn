# Disclosure stdlib

`std/disclosure` renders authorship disclosure artifacts from an
RFC 8693-style `ActorChain` value. Use it when a host or connector needs the
same actor chain represented in a surface-native form such as Git trailers,
a Slack byline, or a GitHub author-mode decision.
Generic ActorChain validation, traversal, and compact formatting live in
[`std/identity`](./identity.md); disclosure reuses that module and adds only
surface-specific rendering policy.

```harn
import { append_git_trailers, render, slack_message_disclosure } from "std/disclosure"

pipeline default() {
  let chain = {
    sub: "user:kenneth",
    act: {sub: "agent:merge-captain", act: {sub: "agent:burin"}},
  }

  let trailers = render(chain, "git")
  let commit_message = append_git_trailers("Fix the merge gate", chain)
  let byline = render(chain, "slack")
  let slack = slack_message_disclosure(chain)
  let github = render(chain, "github")
}
```

Built-in surfaces:

| Surface | Return value |
|---|---|
| `git` | Trailer block string |
| `slack` | Byline string |
| `github` | `{kind: "github_author_choice", author_mode, commit_auth_identity, pull_request_auth_identity, author, co_author, commit_author, trailers, principals, actor_chain}` |

`slack_message_disclosure(chain, options?)` returns the connector-facing Slack
artifact: `{kind: "slack_message_disclosure", byline, customize, ai_mark,
principals, actor_chain}`. Slack connector packages use `customize` only when
the granted scopes include `chat:write.customize`; otherwise they post under
the app identity and render `byline` as text. `ai_mark` is default-on and is
designed to be embedded in surfaces that support machine-readable metadata.

`git_trailers(chain, options?)` is a typed convenience wrapper around the
`git` surface. `append_git_trailers(message, chain, options?)` appends that
block to a commit message or PR body, deduping exact trailer lines. Pass
`{enabled: false}` / `{suppress: true}`, or set those fields on
`[surfaces.git]`, to leave the text unchanged. DCO sign-off stays human-only:
`Signed-off-by:` lines for non-human actor-chain principals are omitted even
when an overlaid template includes them.

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
customize_name_template = "{{ current.label }}"
customize_icon_emoji = ":robot_face:"
ai_mark_enabled = true
ai_mark_event_type = "harn.ai_disclosure.v1"

[disclosure.surfaces.github]
kind = "github_author_choice"
author_mode = "human" # "human" or "bot"
```

The GitHub surface defaults to `author_mode = "human"`. Human mode selects the
origin principal as the commit author, keeps the current agent as `co_author`,
and exposes the Git trailer block that commit and PR adapters should append.
For human mode, commit adapters can send the explicit `commit_author`, while PR
adapters need user auth because GitHub assigns PR authorship from the
authenticated identity. Bot mode selects the current agent as the display
author, sets both auth-identity hints to `github_app`, and sets
`omit_custom_commit_author` so write adapters route through the authenticated
GitHub App `[bot]` identity instead of sending custom author or committer
fields.

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
