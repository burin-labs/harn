# Toolchain config and cache reference

The `package_manager_config` preset admits named tool paths under `~/.config`
and `~/.cache`. Other siblings aren't readable unless you grant them explicitly.
The [credential denylist](./sandbox-read-deny-reference.md) overrides every grant.

The owning list is `package_manager_config_read_roots_for_home` in
`crates/harn-vm/src/stdlib/sandbox/read_roots.rs`. Linux and macOS use that same
list. Windows doesn't confine child processes; these roots don't provide a
security boundary there.

## Measured paths

These Linux probes ran on September 30, 2026, with synthetic configuration and
networking disabled. `strace` recorded filesystem calls inside a Bubblewrap
sandbox. Each command ran with readable XDG directories, then with both
directories inaccessible. A planted file succeeded in the readable control
and returned `EACCES` in the refused control.

Paths below are home-relative. “None” means the trace recorded no config/cache
access in that command, not that every command the tool offers ignores them.
Cache rows name the containing directory; the trace includes both reads and
writes. Refusal results describe these fixtures and installed versions.

| Tool and command | Config paths under `.config/` | Cache paths under `.cache/` | Result with both directories refused |
| --- | --- | --- | --- |
| Git `status --porcelain` | `git/config`, `git/ignore` | None | Uses defaults; succeeds |
| Cargo `check --offline`, Clippy `--offline` | `git/config` | None | Uses defaults; succeeds |
| rustfmt `--check src/main.rs` | `rustfmt/.rustfmt.toml`, `rustfmt/rustfmt.toml` | None | Fails on config metadata lookup |
| npm `run check --offline` | None | None | Succeeds |
| pnpm `run check` | `pnpm/rc` | None | Uses defaults; succeeds |
| Yarn `run check` | `yarn/config`, `yarn/link` | `yarn` | Fails opening config |
| Corepack pnpm/Yarn shims | None | `node/corepack/lastKnownGood.json` | Both baselines need an installed package-manager distribution; refusal returns `EACCES` |
| Bun `run check` | None | None | Succeeds |
| pip `list --disable-pip-version-check` | `pip/pip.conf` | `pip` | Uses defaults and disables cache; succeeds |
| uv `pip list --python /usr/bin/python3` | `uv/uv.toml` | `uv` | Fails initializing cache |
| Go `test ./goproject/...` | `go/env`, `go/telemetry/*` | `go-build` | Fails initializing cache |
| GCC and Clang compile a C file | None | None | Succeed |
| CMake configure | `cmake/api/v1/query`, `cmake/instrumentation-*/v1/query` | None | Ignores unavailable queries; succeeds |
| Ninja and Make build | None | None | Succeed |
| Java `javac Main.java` | None | None | Succeeds |
| Gradle `--offline --no-daemon tasks` | None | None | Succeeds |
| Maven `--offline validate` | None | None | Succeeds |
| sbt `--server -Dsbt.offline=true about` | `sbt/sbtopts`, `sbt/preloaded/*`; Coursier probes `coursier/credentials`, `coursier/credentials.properties`, `coursier/mirror.properties` | `sbt`, `coursier`, `JNA` | Cannot read its cached launcher; fails |
| .NET restore with workspace `NuGet.Config`, then build without restore | None required by the workspace config | None | Succeeds after first-use setup; see limitations below |
| Ruby syntax check and Bundler `check` | None | None | Succeed |
| RubyGems `list --local` | `gem/gemrc` | None | Uses defaults; succeeds |
| Composer `validate --no-check-publish` | `composer/config.json`, `composer/composer.json`, `composer/auth.json`, `git/config` | `composer` | Uses defaults; succeeds |
| Swift `build` | `swiftpm/configuration/{mirrors,registries}.json`, `swiftpm/security`; probes `swiftpm/{cache,config}`, `git/config` | `clang`, `org.swift.swiftpm`, `org.swift.foundation.URLCache` | Fails compiling the manifest without its module cache |
| Julia offline `Pkg.status()` | None | None | Succeeds with an empty local registry |
| mise `ls --offline` | `mise/{config.toml,config.local.toml,mise.toml,mise.local.toml,miserc.toml,conf.d,trusted-configs}` | `mise` | Uses defaults; succeeds |
| Ruff `check main.py` | `ruff/{.ruff.toml,ruff.toml,pyproject.toml}`, `git/{config,ignore}` | None | Uses defaults; succeeds |
| Black `--check main.py` | `black` | `black` | Uses defaults and skips cache; succeeds |
| Prettier `--check main.js`, ESLint `main.js` | None | None | Succeed |

## Grant boundaries

The preset grants the measured tool config directories, plus the single
`coursier/mirror.properties` file and `swiftpm/configuration` directory.
Coursier credential files and SwiftPM's `security` directory aren't admitted.
Composer's `auth.json` stays denied inside its admitted config directory.

Measured caches receive tool-specific read roots. `go-build` and `harn` are
already writable roots owned by `developer_toolchains`, so the package-manager
list doesn't repeat them. On macOS a later read-only rule would cancel their
write grant.

[Workspace toolchain environment](./sandboxing.md#running-real-toolchains-in-the-sandbox)
already redirects writable caches into the workspace. A home-cache refusal in
this census therefore doesn't imply that the corresponding Harn build fails.
The census deliberately measures default paths before that relocation.

## Reproduction and limits

`scripts/sandbox_config_census.harn` contains the commands, fixtures, controls,
and trace parser. It requires Linux, `bwrap`, and `strace`. Its JSON report
records every attempted config path and cache directory, syscall outcomes,
exit status, unavailable tools, and bounded failure diagnostics. It doesn't
save successful tool output or read your home configuration. Java's `user.home` names the
synthetic home; Rust uses the installed toolchain with a private Cargo home.

```sh
harn run --no-sandbox scripts/sandbox_config_census.harn -- /tmp/config-census.json
```

The census isn't a network-installation test. sbt needs preinstalled launcher
and boot artifacts; Julia uses an empty registry. Temporary tool installations
can be exposed with the second script argument. The third argument names a
home directory containing sbt's boot, launcher, and Coursier artifacts; only
those artifact directories are copied. .NET's cold first-use restore timed out in
the initial probes; its subsequent refused-directory build succeeded. That
timeout doesn't prove config refusal was its cause.

The paths were traced on Linux. The live canonical CLI regression also checks
Linux and macOS: admitted config/cache reads succeed, unlisted siblings fail,
and explicit parent grants restore only the unlisted reads. macOS tool defaults
under `~/Library` aren't changed by this narrowing.

The [September 30 evidence](../evidence/9035-config-census.json) records 70
terminal commands, zero pending commands, the expected refused reads, and the
.NET timeout. Corepack's readable baselines lacked a cached distribution;
their traces still recorded `node/corepack/lastKnownGood.json` reads.
