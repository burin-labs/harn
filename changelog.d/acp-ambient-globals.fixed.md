`harn check` no longer reports `HARN-NAM-001` (unresolved value identifier) for
the ambient globals the ACP session executor binds before running a pipeline
(`prompt`, `prompt_content`, `prompt_messages`, `cwd`, `mcp`). The type
checker's ambient-root whitelist and the executor's global bindings now derive
from one source of truth (`harn_parser::acp_ambient_globals`), so a global the
executor injects can never be one the checker rejects.
