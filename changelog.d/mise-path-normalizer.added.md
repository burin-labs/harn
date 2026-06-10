- **`run()` can prepend the repo-declared toolchain to the child-process
  `PATH` (mise/asdf path normalizer).** When `HARN_RUN_TOOLCHAIN_PATH` is set,
  the `run_*` tools detect a repo's declared interpreter versions
  (`.tool-versions`, `.mise.toml`, `.ruby-version`, `.nvmrc`) at or above the
  command's cwd, resolve them via `mise where` / `asdf where`, and prepend the
  resolved bin dir to the child process's `PATH` only — so the command sees the
  version the repo declares instead of a stale system interpreter. Strictly
  declaration-gated (no version file ⇒ `PATH` byte-identical), process-scoped,
  delegates all version resolution to mise/asdf (no per-language table in harn),
  never prepends a path that does not exist, and logs a one-line override notice.
  Generalizes burin-code #2136's keg-only-Ruby hardcode; default OFF and
  per-session disable-able.
