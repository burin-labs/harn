# `std/cli/paths`

Application-scoped config, data, and cache directory helpers for CLI
subcommand scripts.

The module is pure `.harn`. Each helper takes the environment and system
capabilities as explicit parameters rather than reaching for an ambient
harness, and none of them creates a directory.

```harn,check
import {
  xdg_cache_home,
  xdg_config_home,
  xdg_data_home,
} from "std/cli/paths"

fn main(harness: Harness) {
  const config_dir = xdg_config_home(harness.env, harness.system, "harn")
  const data_dir = xdg_data_home(harness.env, harness.system, "harn")
  const cache_dir = xdg_cache_home(harness.env, harness.system, "harn")

  harness.stdio.log("${config_dir} ${data_dir} ${cache_dir}")
}
```

## Surface

| Function | Returns |
| --- | --- |
| `xdg_config_home(env, system, app_name)` | App-specific config directory |
| `xdg_data_home(env, system, app_name)` | App-specific data directory |
| `xdg_cache_home(env, system, app_name)` | App-specific cache directory |

`env` is a `HarnessEnv` and `system` a `HarnessSystem` — pass `harness.env`
and `harness.system`.

`app_name` must be one path segment. Empty names, dot segments, and names
containing `/` or `\` throw rather than allowing a caller to escape the
app-specific directory.

## Resolution

The helpers honor absolute XDG environment variables first, then fall
back to platform conventions. Relative XDG values are ignored, matching
the [XDG Base Directory Specification][xdg-spec].
Fallbacks resolve the user home from `env.get("HOME")`, then
`env.get("USERPROFILE")`; if neither is set to an absolute path,
the helper throws.

| Helper | XDG env | macOS fallback | Other fallback |
| --- | --- | --- | --- |
| `xdg_config_home` | `$XDG_CONFIG_HOME/<app>` | `~/Library/Application Support/<app>` | `$HOME/.config/<app>` |
| `xdg_data_home` | `$XDG_DATA_HOME/<app>` | `~/Library/Application Support/<app>` | `$HOME/.local/share/<app>` |
| `xdg_cache_home` | `$XDG_CACHE_HOME/<app>` | `~/Library/Caches/<app>` | `$HOME/.cache/<app>` |

The macOS locations follow Apple's user-domain guidance for
[application support files and discardable cache files][apple-dirs].

### Inside the sandbox

`harn run` sandboxes the script by default, and the sandbox relocates `HOME`
and sets `XDG_CACHE_HOME` under the workspace. The helpers resolve the
environment they are given, so a sandboxed script sees workspace-local
directories rather than the user's:

| | default `harn run` | `harn run --no-sandbox` |
| --- | --- | --- |
| config | `<workspace>/.harn-toolchain-cache/Library/Application Support/harn` | `~/Library/Application Support/harn` |
| cache | `<workspace>/.harn-toolchain-cache/xdg-cache/harn` | `~/Library/Caches/harn` |

That is the intended isolation: a script cannot reach the real user
directories unless the caller lifts the sandbox. Do not assume a path these
helpers return is shared with other tools on the machine.

These helpers only resolve paths. Call `harness.fs.mkdir(path)` when a
script actually needs to create the directory.

The macOS example values above were produced by running the snippet at the
top of this page with and without `--no-sandbox` on harn 0.10.101.

[xdg-spec]: https://specifications.freedesktop.org/basedir-spec/latest/
[apple-dirs]: https://developer.apple.com/library/archive/documentation/FileManagement/Conceptual/FileSystemProgrammingGuide/MacOSXDirectories/MacOSXDirectories.html
