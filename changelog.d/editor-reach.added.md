- Reachable editor support: an `Editor setup` guide with copy-pasteable
  Neovim and Zed configuration that points a generic LSP client at the
  `harn-lsp` binary already installed on `PATH`, plus VS Code extension
  install steps. A new `publish-vscode.yml` release workflow packages the
  `editors/vscode` extension into an installable `.vsix` artifact on every
  tag and publishes it to the VS Code Marketplace and Open VSX once a
  maintainer sets the `VSCE_PAT` / `OVSX_PAT` repo secrets — inert (never
  CI-failing) until then.
