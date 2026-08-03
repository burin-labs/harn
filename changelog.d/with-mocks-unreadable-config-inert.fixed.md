- **The retired `with_mocks` migration no longer silently disarms fixtures at a
  call site whose config it cannot read.** `with_mocks` read `host_mocks` and
  `llm_mocks`; `with_scenario` reads `capabilities` and `llm`. Keys can only be
  rewritten on a dict literal, so renaming the callee over a forwarded
  parameter, a helper call, or a config carrying an unrecognized key left
  `with_scenario` reading two fields that were not there — both scopes
  installed empty and the body ran against the real host, while the retired call
  site was gone and the plan converged. Those call sites now stay inert for a
  human.
