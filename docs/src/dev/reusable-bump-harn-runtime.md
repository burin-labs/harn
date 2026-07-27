# Reusable "bump Harn runtime" workflow

Every Harn package repo pins the Harn runtime it builds against in a
`.harn-version` file. Keeping that pin current used to mean copying a large
`bump-harn.yml` state machine into each repo. Harn now publishes **one**
reusable workflow so a package needs only a small trigger wrapper plus its own
declared refresh and validation commands.

- Workflow: `.github/workflows/bump-harn.yml` (`workflow_call`).
- Orchestration: `scripts/bump_harn_runtime.harn` → `std/bump/runtime` (pure
  state machine) → `std/bump/live` (git and authenticated `gh` calls via
  `std/command`).
- Receipt schema: `harn-bump-runtime-v1` (printed to stdout; key fields also
  land as step outputs and a step-summary block).

## Minimal caller workflow

Drop this into the consuming repo. The only repo-specific parts are the trigger
schedule and, when the default lock refresh is insufficient, the
`refresh-command` and `validate-command`.

```yaml
name: Bump Harn Runtime
on:
  workflow_dispatch:
    inputs:
      version:
        description: "Optional Harn tag (vX.Y.Z). Defaults to latest release."
        required: false
        type: string
  schedule:
    - cron: "43 9 * * *"

permissions:
  contents: write
  pull-requests: write

jobs:
  bump:
    uses: burin-labs/harn/.github/workflows/bump-harn.yml@<pinned-sha>
    with:
      version: ${{ inputs.version }}
      # Optional repository-owned materialization. The target tag is inherited
      # as HARN_BUMP_TARGET_TAG. All mutations are included in the one signed
      # commit; a non-zero exit blocks it.
      refresh-command: |
        harn install --locked
        ./scripts/regenerate-derived-sources "$HARN_BUMP_TARGET_TAG"
      # Repository-owned verification runs after every mutation. A non-zero
      # exit blocks the commit.
      validate-command: |
        set -euo pipefail
        mapfile -t files < <(git ls-files '*.harn')
        if (( ${#files[@]} > 0 )); then
          harn fmt "${files[@]}"
          harn check "${files[@]}"
          harn lint "${files[@]}"
        fi
        [ -d tests ] && harn test tests/ --parallel || true
    secrets:
      app-client-id: ${{ secrets.RELEASE_APP_CLIENT_ID }}
      app-private-key: ${{ secrets.RELEASE_APP_PRIVATE_KEY }}
```

Pin `@<pinned-sha>` to a full commit SHA of `burin-labs/harn`. That immutable
ref owns the reusable workflow contract. The workflow then checks Harn out at
the resolved release tag, so its orchestration script, embedded `std/bump/*`
modules, checksum-verifying `setup-harn` action, and installed CLI all come
from the same published version.

## Idempotency and concurrency

- Already current: the pin already matches the resolved target → clean no-op,
  zero mutation.
- Not yet published: a target whose release is still finalizing is a clean exit
  (`outcome: not_ready`); the next scheduled run picks it up.
- Stale heads: an open bump PR with auto-merge armed is **disarmed before** the
  bump branch is reset, so a stale head cannot merge mid-refresh and race a
  duplicate onto the base branch. Concurrent/scheduled reruns are safe.

## Version availability

The orchestration modules (`std/bump/*`) are embedded in the Harn CLI, so the
workflow runs the state machine under the **target** Harn release. The feature
ships in the release identified in `changelog.d/5299.added.md`; bumping to any
release at or after that version works. (Bumps are always forward to the latest
release, so this holds in practice.)

## Security boundary

- **Least privilege, short-lived credentials.** The workflow mints a GitHub App
  installation token scoped to `contents: write` + `pull-requests: write` for
  the run only. The caller passes the App client id and private key as
  `secrets`; no long-lived PAT is used.
- **Signed commits.** The bump commit is created through GitHub's
  `createCommitOnBranch` GraphQL mutation under the App identity, so GitHub
  signs it and an org `required_signatures` ruleset is satisfied. A local
  `git commit` + push would land unsigned and be rejected.
- **Immutable supply chain.** Third-party actions are pinned by full commit SHA.
  The workflow contract is pinned by full Harn commit SHA; first-party runtime
  pieces (orchestration script, embedded modules, `setup-harn`) come from the
  resolved immutable release tag. `setup-harn` verifies the downloaded release
  archive against its published SHA-256 before installing.
- **No package-domain logic in the shared workflow.** The reusable workflow
  never encodes a package's code-generation or build/test knowledge. Repos
  expose their existing owner commands through `refresh-command` and
  `validate-command`; the shared workflow only sequences them. Consumers copy
  no orchestration, release-readiness, signing, branch, or PR machinery.
- **Sandbox posture.** The orchestration runs under `harn run --no-sandbox`
  because it must reach git, the GitHub API through `gh`, and the caller's
  refresh and validation commands. It carries no secret beyond the scoped
  installation token, which is passed via the environment and never written to
  the repo.
