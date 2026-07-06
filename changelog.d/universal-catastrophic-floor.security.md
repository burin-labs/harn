- **Universal catastrophic-command floor at the process chokepoint.** The
  never-approvable catastrophic-command floor is now enforced unconditionally
  at the shared `spawn_process` chokepoint that every hostlib process tool
  funnels through (`run_command`, `run_test`, `run_build_command`,
  `manage_packages`, and the long-running background path) — closing a gap
  where a bare `run_command` (the agent's real shell tool) could run
  machine/disk/data-destroying commands with no floor. Catastrophic reasons are
  now split into two categories: the **universal** destruction set (`rm -rf`
  escaping the workspace, fork bombs, `mkfs`, `dd of=<device>`, `chmod -R 000`,
  `truncate -s 0` of a source file, redirect-over-source, project-root delete)
  is blocked everywhere, including with no `command_policy` on the stack, while
  the recoverable git **workflow** family (`git reset --hard`, `git clean -fd`,
  force-push) stays enforced only when a policy is pushed — so the stdlib
  `git.push --force-with-lease` flow and standalone scripts keep working. The
  same universal backstop also now guards the `process.exec` no-policy path, and
  the shared classifier is exposed as
  `harn_vm::orchestration::universal_catastrophic_reason` so embedders no longer
  need to re-plumb the floor. Policy-present behavior is unchanged.
