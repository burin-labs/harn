- **Project scanning reports whether test-command discovery completed.**
  Readable scans find root-level Python tests, including tests that do not
  import pytest. Inaccessible paths and unresolved manifests report an
  unavailable result rather than an apparently empty set of commands.

  Migration: replace `project.test_commands` and
  `project.detected_test_command` with `project.test_command_discovery`.
  When `state` is `complete`, read `commands` and nullable `preferred`;
  when it is `unavailable`, report `detail` and do not treat it as empty.
