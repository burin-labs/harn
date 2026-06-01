- **`std/testing` gains LLM-mock builders, error assertions, and
  filesystem fixtures.** New helpers cut the boilerplate that kept most
  fixtures on the raw, unscoped form:
  - LLM turn builders — `llm_text`, `llm_done`, `llm_error`,
    `llm_tool_call`, `llm_tool_calls`, and `with_llm_script` — make the
    already-scoped `with_llm_mocks` readable, replacing the
    `llm_mock_clear()` + sequential `llm_mock({...})` pattern.
  - Error assertions — `assert_throws`, `assert_error_contains`, and
    `assert_no_throw` — collapse the
    `try {...}` + `is_err` + `unwrap_err` + `to_string` + `contains`
    chain into one call.
  - Filesystem fixtures — `with_temp_dir`, `with_fs`, and the unified
    `with_scenario` — give a scoped temp workspace (optionally seeded
    from a `{path: contents}` dict) with guaranteed recursive cleanup,
    the fs counterpart to `with_host_mocks`.
