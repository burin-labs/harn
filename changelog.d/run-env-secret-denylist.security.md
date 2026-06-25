- `run`/`command_run` no longer leak secret-bearing environment variables to
  spawned child processes (and thus to the model, which reads child stdout as
  the tool result). Under the default `inherit_clean`/`patch` env modes the
  child environment now strips provider `*_API_KEY`s, `*_TOKEN`/`*_SECRET`/
  `*_KEY` variables, and explicit names like `GITHUB_TOKEN`,
  `HARN_CLOUD_API_KEY`, `BURIN_ADMIN_TOKEN`, and `AWS_SECRET_ACCESS_KEY`. Benign
  build/toolchain vars (`PATH`, `HOME`, `LANG`, `CARGO_*`, …) are preserved, and
  an explicit caller-supplied `env` is left untouched.
- The file-backed OAuth/MCP token store now writes its sealed token file with
  `0o600` permissions on Unix so a wide umask can't leave it group/world
  readable.
