- **Capability migrations now respect lexical callback bindings.** `harn fix`
  no longer mistakes local `call` or `git` callbacks for retired ambient
  builtins, including callbacks unpacked from imported enum variants. `harn
  check` uses the same resolution, so unattended runtime upgrades do not invent
  LLM or tool authority or leave false ambient-call diagnostics behind.
