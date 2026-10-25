Confined commands preserve `PNPM_HOME`, so `pnpm run` can use an already
installed pinned pnpm version offline. Toolchain read and write permissions
and workspace-local build caches keep their existing policy.
