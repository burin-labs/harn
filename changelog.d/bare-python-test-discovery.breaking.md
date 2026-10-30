- **Project scanning reports whether test-command discovery completed.**
  Readable scans find root-level Python tests, including tests that do not
  import pytest. Inaccessible paths and unresolved manifests report an
  unavailable result rather than an apparently empty set of commands. Use
  `harness.scanner.discover_test_commands` for a fresh result without building
  or persisting a full project index. Hosts can supply test-file conventions
  as `layout_profiles`; discovery reports matched layouts and refuses to infer
  a partial suite when several languages match without a root `test` command.

  Migration: replace `project.test_commands` and
  `project.detected_test_command` with `project.test_command_discovery`.
  When `state` is `complete`, read `commands`, nullable `preferred`, and `layout`;
  when it is `unavailable`, report `detail` and do not treat it as empty.
