- **Linux process sandbox no longer fails to spawn when a read-root resolves
  to a regular file.** `process.exec` under the `worktree`/`os_hardened`
  profiles built a Landlock `PATH_BENEATH` rule with directory-only access
  rights (`READ_DIR`, the `MAKE_*`/`REMOVE_*` family, `REFER`) even when the
  root was a *file* — e.g. the package-manager-config preset's `~/.gitconfig`,
  `~/.cargo/config.toml`, or `~/.npmrc`. On a kernel with Landlock support the
  kernel rejects such a rule with `EINVAL`, surfacing as
  `host_call process.exec: Invalid argument (os error 22)`. The directory-only
  bits are now stripped for non-directory rule targets, so the remaining
  file-applicable rights (`READ_FILE`/`EXECUTE`/…) are still enforced and the
  spawn succeeds.
