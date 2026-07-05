- Added a never-approvable **catastrophic command floor** to `command_policy`.
  The deterministic command-risk scanner now emits a distinct `catastrophic`
  label for irreversible destruction — fork bomb; `git reset --hard`;
  `git clean -fd`; `git push --force`/`-f`/`--force-with-lease`; `rm -rf`
  escaping the workspace root or wiping it in place; `dd of=…`; `mkfs`;
  `chmod -R 000`; `truncate -s 0` of a source file; and `>`/`>>` redirection
  onto a source file — detected through adversarial quoting, chained-command
  splitting, `bash -c` recursion, and the `sudo`/`env`/`nice`/`nohup`/`time`/
  `timeout`/`command`/`builtin` wrapper family. A `catastrophic` command is
  always hard-denied (a `status: "blocked"` envelope, no child spawned)
  regardless of policy configuration and is never routed to the consent gate —
  it cannot be approved. Policies may promote other scanner labels to the same
  never-approvable tier with a new `command_policy({deny_labels: […]})` set.
  Also fixed the `destructive` heuristic to key on `dd of=` (a raw overwrite)
  instead of the read-only `dd if=`.
