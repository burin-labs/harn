- **Project-declared host mocks no longer make unavailable operations appear callable.**
  `harn test` accepts mocks for manifest-declared operations without changing
  `host_has`, while undeclared operation typos still fail closed.
