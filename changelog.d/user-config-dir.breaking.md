- **Windows: user-level `providers.toml`, `mcp_presets.toml`,
  `mcp_bulk_auth.toml`, and `skills/` now live in `%APPDATA%\Harn`**, beside
  `config.toml`, which already did. They were read from
  `%USERPROFILE%\.config\harn`, so `harn config` and the runtime disagreed about
  where `providers.toml` was.

  Migration: on Windows, move those files from `%USERPROFILE%\.config\harn` to
  `%APPDATA%\Harn`. macOS and
  Linux are unaffected unless `XDG_CONFIG_HOME` is set, in which case the files
  are now read from `$XDG_CONFIG_HOME/harn`, as `config.toml` already was.

  ```powershell
  Move-Item "$env:USERPROFILE\.config\harn\*" "$env:APPDATA\Harn\"
  ```
