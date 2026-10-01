- `harness.process.run`, hostlib `run_command`, and the other hostlib process
  tools now throw Harn's typed `sandbox_mechanism_unavailable` refusal when a
  spawn needs a platform sandbox mechanism the host lacks (for example, Linux
  without Landlock under `os_hardened`). They used to flatten it into a
  runtime-error or `backend_error` string, so a `catch` could only
  substring-match it.
