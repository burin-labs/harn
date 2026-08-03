Claude sessions no longer block on the compiling parts of dev setup. The
SessionStart hook waits only for the `bootstrap` profile, which configures git
hooks, merge drivers, and the per-worktree Cargo target, and warms tool
installs, the portal build, the workspace check, and local signing in the
background. Sessions in a checkout that tracks `main` previously waited through
a full setup whenever `Cargo.lock` or a `package-lock.json` moved, which was
measured at up to 432s.
