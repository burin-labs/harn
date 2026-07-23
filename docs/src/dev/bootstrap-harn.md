# Bootstrap an exact Harn release

`scripts/bootstrap_harn.mjs` is Harn's supported host-native bootstrap
interface for local development and CI systems outside GitHub Actions. It uses
only Node.js built-ins, runs on Linux, macOS, and Windows, and never requires a
POSIX shell. Node.js 20 or newer is required.

Pin the bootstrap script itself by checking out an immutable Harn release or
commit, then provide either an exact version or a file containing one:

```text
node path/to/harn/scripts/bootstrap_harn.mjs --version 0.10.31 --cache-dir path/to/cache --install-dir path/to/tools/harn
```

```text
node path/to/harn/scripts/bootstrap_harn.mjs --version-file .harn-version --cache-dir path/to/cache --install-dir path/to/tools/harn
```

The effective request must resolve to one `MAJOR.MINOR.PATCH` value, with an
optional leading `v`. `latest`, version ranges, and prerelease selectors are
rejected. When neither option is written on the command line, `--version-file`
defaults to `.harn-version` in the working directory; a missing or invalid file
is a closed failure.

The command prints one JSON receipt to standard output:

```json
{
  "schema_version": "harn-bootstrap-v1",
  "version": "0.10.31",
  "target": "aarch64-apple-darwin",
  "binary_path": "/ci/tools/harn/harn",
  "source": "https://github.com/burin-labs/harn/releases/download/v0.10.31/harn-aarch64-apple-darwin.tar.gz",
  "checksum": "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
  "cache_hit": false,
  "binary_sha256": "abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789"
}
```

The first seven fields form the stable `harn-bootstrap-v1` contract.
`binary_sha256` provides additional local integrity evidence. The installed
directory also contains the integrity-only `install-manifest.json` used to
validate future reuse; the JSON printed for each invocation is its audit
receipt.

## Verification and atomicity

Every online run downloads the exact release's `SHA256SUMS`, requires one
unambiguous entry for the selected platform archive, and verifies the archive
before extraction. A cached archive is only a candidate: the bootstrapper
hashes it again against the release metadata before reporting
`cache_hit: true`. If checksum metadata previously cached for an exact release
differs from newly published metadata, the run fails closed.

Archive files and install directories are prepared beside their destinations
and published atomically only after verification. Interrupted temporary paths
never become cache or install state. Concurrent processes installing the same
version may both do work, but they converge on one complete, validated
installation; a process that loses publication validates the winner before
returning.

The default cache is under the runner tool cache, `$XDG_CACHE_HOME`, Windows
`%LOCALAPPDATA%`, or the user's `.cache` directory. The default installation
is versioned below that bootstrap cache. Use `--cache-dir` and `--install-dir`
in shared CI so Harn does not write to a package manager, Cargo home, or another
toolchain's directory.

## Offline runs

After one successful online install, `--offline` requires both of these exact
version entries:

- `metadata/<version>/SHA256SUMS`
- `downloads/<version>/<platform archive>`

Offline mode performs no network request and re-hashes the archive against the
cached checksum metadata. A missing or corrupt entry is a closed failure; the
bootstrapper never selects another version or falls back to an unverified
binary.

## Proxies

The bootstrapper uses Node's built-in `fetch`. Node.js 22.21+, 24.5+, and newer
release lines can read proxy settings when `NODE_USE_ENV_PROXY=1` is set before
Node starts. Configure `HTTPS_PROXY`, `HTTP_PROXY`, and `NO_PROXY` in the
process environment. Harn's setup action enables this support automatically
when the runner's Node version provides it. On older Node versions, use a
network environment that provides direct GitHub access or pre-populate the
exact cache on a connected machine and run with `--offline`.

## Failure recovery and source builds

Rerun the same command after a network interruption. Unique
`.harn-bootstrap-*` leftovers are ignored and can be deleted after no bootstrap
processes are active. If an operator must rebuild one entry, remove only that
version's metadata/archive directory and its versioned install directory, then
rerun online.

The bootstrapper does not silently build from source. A source build has
different provenance, prerequisites, and cache semantics, so it must be an
explicit policy decision. The supported fallback is to install the same exact
version with Rust:

```text
cargo install --locked harn-cli --version 0.10.31
```

Alternatively, check out the matching `v0.10.31` source tag and run
`cargo install --locked --path crates/harn-cli`. Neither source-build path
emits a `harn-bootstrap-v1` receipt or counts as a release-archive cache hit.

## GitHub Actions

The first-party setup action is a thin adapter over this same file. It resolves
the version and platform through the bootstrapper, restores the candidate
cache, invokes the bootstrapper for verification and installation, adds the
binary directory to `PATH`, and exposes the JSON as its `receipt` output:

```yaml
- uses: burin-labs/harn/.github/actions/setup-harn@<full-commit-sha>
  id: harn
  with:
    version: 0.10.31
- env:
    HARN_RECEIPT: ${{ steps.harn.outputs.receipt }}
  run: node -e 'process.stdout.write(process.env.HARN_RECEIPT)'
```
