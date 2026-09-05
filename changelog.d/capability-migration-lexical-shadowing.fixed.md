- **Capability migrations now respect lexical callback bindings.** `harn fix`
  no longer mistakes local `call` or `git` callbacks for retired ambient
  builtins, so unattended runtime upgrades do not invent LLM or tool authority.
