- Fixed the reusable `bump-harn` workflow for external (fleet) callers. The
  "Checkout Harn runtime orchestration" step pinned the `burin-labs/harn`
  checkout to `github.workflow_sha`, which for any caller other than
  `burin-labs/harn` itself resolves to the *caller's* commit — a ref that does
  not exist in `burin-labs/harn` — so every downstream bump failed with
  `fatal: remote error: upload-pack: not our ref`. The step now resolves the
  target release first and checks the orchestration + `setup-harn` action out at
  that version tag, which is always a valid `burin-labs/harn` ref and keeps the
  orchestration version-consistent with the Harn being installed.
