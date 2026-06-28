- The default package index now resolves from `https://packages.harnlang.com/harn-package-index.toml`, and
  `harn publish` opens its index PR against the public `burin-labs/harn-packages` repo (previously the index
  was served from a private repo's GitHub Pages). Override per-command with `--registry` / `HARN_PACKAGE_REGISTRY`
  for resolution, or `--index-repo` / `--index-path` for publishing.
