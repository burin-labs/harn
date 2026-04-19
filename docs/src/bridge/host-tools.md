# Host tools over the bridge

`host_tool_list()` and `host_tool_call(name, args)` are the host-side
mirror of Harn's LLM-facing `tool_search` flow: the script can ask the
host what tools exist right now, inspect their schemas, and invoke the
one it actually needs.

This is useful when the host owns the real capabilities:

- Claude Code style tools such as `Read`, `Edit`, and `Bash`
- IDE actions such as `open_file`, `ide.panel.focus`, or
  `ide.git.worktree`
- product-specific actions that vary by project, session, or user role

## Worked example

The script below discovers a readable tool at runtime, refuses to use a
deprecated one, and then calls it with a single structured argument
payload.

```harn
import { host_tool_available, host_tool_lookup } from "std/host"

pipeline inspect_readme(task) {
  if !host_tool_available("Read") {
    log("Host does not expose a Read tool in this session")
    return nil
  }

  let read_tool = host_tool_lookup("Read")
  assert(read_tool != nil, "Read tool metadata should be present")
  assert(read_tool?.deprecated != true, "Read tool is deprecated on this host")

  let result = host_tool_call("Read", {path: "README.md"})
  log(result)
}
```

What happens at runtime:

1. `host_tool_list()` sends `host/tools/list` to the active bridge host.
2. The host replies with tool descriptors: `name`, `description`,
   `schema`, and `deprecated`.
3. `host_tool_call("Read", {path: "README.md"})` reuses the bridge's
   existing `builtin_call` path, so the host receives the dynamic tool
   invocation without Harn needing a second bespoke call protocol.

## Shape conventions

Harn normalizes each entry returned by `host/tools/list` to this form:

```json
{
  "name": "Read",
  "description": "Read a file",
  "schema": {
    "type": "object",
    "properties": {
      "path": {"type": "string"}
    },
    "required": ["path"]
  },
  "deprecated": false
}
```

That means scripts can safely branch on `tool.schema` or
`tool.deprecated` without having to care whether the host originally
used compatibility field names such as `short_description` or
`input_schema`.

## Notes

- Without a bridge host, `host_tool_list()` returns `[]`.
- `host_tool_call(...)` requires an attached bridge host and throws if
  none is active.
- Hosts remain authoritative: if a tool disappears between discovery and
  invocation, the host error is surfaced to the script normally.
