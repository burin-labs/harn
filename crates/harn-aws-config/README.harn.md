# Captured AWS client environment

This publishable crate preserves the AWS SDK config and credential provider
implementation from `aws-config` 1.12.0. Harn needs its complete credential and
region chains to read a captured session environment. Upstream's environment
constructor and region configuration method are crate-private, so public
builders cannot supply every declared input, including the ECS authorization
token. Mutating process environment would mix credentials between parallel
engines.

The recorded `upstream.patch` exposes the existing environment and region
configuration seams, centralizes value-free environment Debug formatting, and
gives profile credential helpers the same captured environment. Helpers preserve
the launcher's PATH and ordinary system inputs, without reading another
session's process-global credentials. Package identity is `harn-aws-config`.
Harn VM aliases this package as
`aws-config`, so Cargo's published dependency graph carries the same behavior to
embedders. A root-only Cargo patch would disappear from that graph.

Upstream authorship, Apache-2.0 license, features, and SDK chain implementation
remain in their original files. The published upstream archive omits the fixture
directory used by its internal unit tests. Those unavailable tests and upstream
doctests are disabled in the package manifest. Their development dependencies
and example target are omitted; runtime dependencies and features are preserved.
Harn's Bedrock tests exercise the
actual credential and region chains with captured static, profile, and local ECS
inputs. They must prove that distinct parallel sessions retain distinct inputs.

`upstream.json` identifies the pristine crates.io archive and the recorded patch.
The owning verification command verifies the downloaded archive's SHA-256,
applies the recorded patch to its extracted files, and compares the complete
mirror. It rejects additional or modified SDK files. Updating the SDK requires a
new verified archive identity and an explicitly reviewed patch.

Cargo reserves `.cargo_vcs_info.json` and `Cargo.toml.orig` for its own package
writer. The mirror keeps those pristine files for the complete source comparison
but excludes them from package inputs. `Cargo.toml.upstream` preserves the
original upstream manifest inside the published crate.

`Cargo.lock.upstream` preserves the pristine upstream lockfile as recorded
provenance. It is not an active Cargo lockfile: the Harn workspace lockfile owns
the mirrored component's runtime dependency resolution and updates. The recorded
patch renames it without changing its bytes, so the mirror does not introduce a
second dependency update owner.
