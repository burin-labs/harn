- Closed a filesystem write/delete symlink-swap TOCTOU in the deterministic
  `tools/{write_file, delete_file}` builtins (audit finding F5). The
  workspace-scope check canonicalizes a copy of the path, but the subsequent
  `write`/`remove_*` ran on the raw path and followed a symlink at the final
  component at op time, so an in-workspace attacker could swap a check-passed
  path for a symlink pointing outside the allowed roots and escape the
  workspace. `write_file` now opens the final component with `O_NOFOLLOW` on
  Unix (with an `lstat`-reject fallback elsewhere) so a symlink-final path is
  rejected at open time rather than followed; `delete_file` re-validates an
  escaping final-component symlink under the active policy and refuses to
  remove through it. Normal in-root writes, overwrites, and deletes of real
  files are unaffected.
