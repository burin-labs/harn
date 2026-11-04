A confined child's temp dir and toolchain cache no longer live inside the workspace. They moved from `.harn-tmp`
and `.harn-toolchain-cache` in the first workspace root to a per-workspace runtime dir in the user cache directory
(`~/Library/Caches/harn-sandbox/<digest>` on macOS, `$XDG_CACHE_HOME/harn-sandbox/<digest>` or
`~/.cache/harn-sandbox/<digest>` elsewhere), so a linter or test collector walking the project no longer finds the
child's temporary files. The runtime dir is owner-only, refuses a symlinked or foreign-owned component, and is
granted to every backend, including runs restricted to declared roots. A root that covers a workspace still covers
its temp dir, so a policy narrowed to a directory made there stays inside the enclosing policy. A script's own scratch
from `harness.fs.workspace_temp_dir()` and `mkdtemp_in_workspace` stays in `.harn-tmp` inside the workspace. Existing
`.harn-toolchain-cache` directories are no longer used and can be deleted.
