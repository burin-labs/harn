- `harn providers export` / `harn providers validate` accept `--capabilities-overlay <path>` (capabilities.toml
layout): overlay-declared private/local models can claim structured capabilities (native tools, vision, prompt
caching, reasoning modes) in the exported artifact instead of relying on legacy `capabilities` tags or post-export
patching. The serve runtime honors the same data through the manifest `[capabilities]` section, so exported and
served catalogs agree; new `harn_vm::llm::capabilities::parse_capabilities_toml` parses without mutating thread
state (#3267)
