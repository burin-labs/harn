# `std/cli/paths`

Application-scoped config, data, and cache directory helpers for CLI
subcommand scripts.

The module is pure `.harn`; it reads environment variables through
`harness.env` and never creates directories.

```harn
import {
  xdg_cache_home,
  xdg_config_home,
  xdg_data_home,
} from "std/cli/paths"

const config_dir = xdg_config_home("harn")
const data_dir = xdg_data_home("harn")
const cache_dir = xdg_cache_home("harn")
```

## Surface

| Function | Returns |
| --- | --- |
| `xdg_config_home(app_name)` | App-specific config directory |
| `xdg_data_home(app_name)` | App-specific data directory |
| `xdg_cache_home(app_name)` | App-specific cache directory |

`app_name` must be one path segment. Empty names, dot segments, and names
containing `/` or `\` throw rather than allowing a caller to escape the
app-specific directory.

## Resolution

The helpers honor absolute XDG environment variables first, then fall
back to platform conventions. Relative XDG values are ignored, matching
the [XDG Base Directory Specification][xdg-spec].
Fallbacks resolve the user home from `harness.env.get("HOME")`, then
`harness.env.get("USERPROFILE")`; if neither is set to an absolute path,
the helper throws.

| Helper | XDG env | macOS fallback | Other fallback |
| --- | --- | --- | --- |
| `xdg_config_home("harn")` | `$XDG_CONFIG_HOME/harn` | `~/Library/Application Support/harn` | `$HOME/.config/harn` |
| `xdg_data_home("harn")` | `$XDG_DATA_HOME/harn` | `~/Library/Application Support/harn` | `$HOME/.local/share/harn` |
| `xdg_cache_home("harn")` | `$XDG_CACHE_HOME/harn` | `~/Library/Caches/harn` | `$HOME/.cache/harn` |

The macOS locations follow Apple's user-domain guidance for
[application support files and discardable cache files][apple-dirs].

These helpers only resolve paths. Call `harness.fs.mkdir(path)` when a
script actually needs to create the directory.

[xdg-spec]: https://specifications.freedesktop.org/basedir-spec/latest/
[apple-dirs]: https://developer.apple.com/library/archive/documentation/FileManagement/Conceptual/FileSystemProgrammingGuide/MacOSXDirectories/MacOSXDirectories.html
