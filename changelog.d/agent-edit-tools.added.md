- **Default mutation toolset.** `std/agent/host_tools` now ships
  `agent_edit_tools(...)`, the canonical root-scoped `write_file`, `edit_file`,
  `create_directory`, and `delete_path` tools every embedder previously
  hand-rolled. They wrap the existing hostlib filesystem primitives, reuse the
  same root resolution and path-scope enforcement as `agent_read_tools`, and are
  annotated as mutating (`kind: edit|delete`, `side_effect_level:
  workspace_write`) so the read-only stance hides them. Compose explicitly over
  the read/command surface (`agent_edit_tools(agent_host_tools(nil, opts),
  opts)`); mutation stays opt-in.
