- **Project-aware startup is demand-driven (#7314).** Ordinary `harn run`
  entrypoints no longer materialize unreachable dependencies, construct
  connector registries, or parse unused hook handlers. Package imports and
  connector calls initialize their owning state on first use, matching policy
  hook failures still stop dispatch, and `--eager-project-handlers` retains
  fail-fast validation for the complete project. Cached package graphs
  revalidate current manifest and lock authority before execution.
