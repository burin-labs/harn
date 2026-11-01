- **Confined children run a project's installed pinned pnpm.** When `package.json` pins `packageManager: "pnpm@X"`
  and X is installed under `$PNPM_HOME/.tools`, it comes first on the child's PATH, so `pnpm` no longer needs the
  registry to switch versions and works without network.
