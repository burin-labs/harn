# Bootstrap an exact Harn release

Check out an immutable Harn release or commit, then run its bootstrap adapter:

```sh
sh path/to/harn/scripts/bootstrap-harn.sh --version 0.10.31 --cache-dir path/to/cache --install-dir path/to/tools/harn
```

Use `--version-file .harn-version` instead of `--version` to read the pin from
a file. The effective version must be exactly `MAJOR.MINOR.PATCH`, optionally
prefixed with `v`; ranges, `latest`, and prereleases are rejected.

The shell adapter needs curl or wget, a SHA256 utility, and tar. On Windows,
run it in Git Bash; ZIP seed extraction uses PowerShell. Node is unnecessary.
`.harn-bootstrap-version` pins the seed independently of the requested runtime,
so the same seed can install older Harn releases.

The adapter runs the checked-in `std/runtime/bootstrap` source, which delegates
release verification and cache policy to `std/runtime/install`. Native archive
installation is performed by `harn upgrade --archive`, with a required SHA256.
The command prints one `harn-bootstrap-v1` JSON receipt containing `version`,
`target`, `binary_path`, `source`, `checksum`, `cache_hit`, `binary_sha256`, and
`checksum_source`. The destination's `install-manifest.json` records the native
installation receipt.

## Cache and offline use

Online installs refresh the release's checksum manifest and reject changes to
previously cached metadata. While the aggregate manifest is unpublished, a
verified GitHub release-asset digest can authorize installation; a later
manifest must agree with that digest. Downloads become cache entries only after
native verification succeeds. Cached archives are reverified before use.

After an online installation, pass `--offline` to require the cached manifest
and archive without network access. A digest-only cache is insufficient for
offline use. Seed bootstrap also needs its cached manifest and archive; set
`HARN_BOOTSTRAP_OFFLINE=1` to apply offline mode before Harn itself starts.

The runtime cache defaults to the runner tool cache, `$XDG_CACHE_HOME`, Windows
`%LOCALAPPDATA%`, or the user's `.cache` directory. Use explicit `--cache-dir`
and `--install-dir` in shared CI. Configure `HTTPS_PROXY`, `HTTP_PROXY`, and
`NO_PROXY` when a proxy is required.

## Recovery

Rerun the same command after a network interruption. Corrupt runtime archives
are replaced online; offline verification fails closed. Installation writers
use an operating-system file lock, released automatically when the process
exits. The executable and aliases are published atomically, followed by the
receipt. Do not delete an active installation's `.harn-install.lock` file.

There is no automatic source-build fallback. To choose different provenance,
install the exact version explicitly with `cargo install --locked harn-cli
--version 0.10.31`; that path does not produce a release-archive receipt.

## GitHub Actions

The first-party action resolves the runtime, restores only its version and
platform's archive candidates, invokes the shared installer, and exposes the
receipt while adding the installed binary to `PATH`:

```yaml
- uses: burin-labs/harn/.github/actions/setup-harn@<full-commit-sha>
  id: harn
  with:
    version: 0.10.31
- env:
    HARN_RECEIPT: ${{ steps.harn.outputs.receipt }}
  run: printf '%s\n' "$HARN_RECEIPT"
```
