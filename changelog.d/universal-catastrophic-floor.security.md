- **Universal catastrophic-command floor for `host_process_exec`.** The
  never-approvable catastrophic-command floor is now a universal backstop: a
  bare `process.exec` / `process.spawn` with **no** `command_policy` pushed can
  no longer run machine/disk/data-destroying commands (`rm -rf /` and other
  workspace escapes, fork bombs, `mkfs`/`dd of=<device>`, `chmod -R 000`,
  `truncate -s 0` of a source file, redirect-over-source, project-root delete).
  Catastrophic reasons now carry a category: the **universal** (destruction)
  set is enforced everywhere, including the no-policy path, while the
  recoverable git **workflow** family (`git reset --hard`, `git clean -fd`,
  force-push) remains enforced only when a policy is on the stack — so the
  stdlib `git.push --force-with-lease` flow and standalone scripts keep working.
  Policy-present behavior is unchanged.
