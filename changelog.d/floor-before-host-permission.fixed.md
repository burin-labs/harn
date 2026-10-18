- **A command on the never-approvable floor no longer asks a host for approval.** When an approval policy says
  "ask" for a tool call whose command the catastrophic floor (or a `deny_labels` entry) refuses, such as
  `git push origin +main` or `git reset --hard`, dispatch now returns a terminal `deterministic_precheck` denial
  naming the floor instead of sending `session/request_permission` for a command that could never run.
