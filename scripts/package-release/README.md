# Source package candidates

This driver creates a package archive, unpacks it, and tests those exact bytes
before it writes a release candidate. A candidate is the set of files a later
publisher may release. The archive is never rebuilt after verification.

Harn's package tools own packing, the input census, test discovery and strict
verification. The driver retains the complete reports. It rejects missing tests,
unreported gates, changed inputs and a changed archive. A failure keeps the
required count and the names of pending or failing checks. Missing evidence
stays absent.

The shared workflow repository owns the candidate manifest and the publisher.
This driver imports that manifest contract from a locked public dependency.
It records the archive, checksums, package metadata, verification reports and
release notes for that publisher. It has no release-writing credential and
doesn't merge a change, advance a checkpoint or deploy an artifact.

The caller supplies its actual workflow source, run and attempt. The requested
source must match both that identity and the clean Git checkout. The package
name must match the product the caller selected. Package names and commands
are data; the build function has no organization-specific branch.

The promotion reader uses the published GitHub connector. It accepts only a
successful package build on main at the requested source and attempt. It reads
the candidate manifest first, then checks the metadata file against that
manifest's hash. A small metadata artifact carries the same file bytes as the
release-files artifact. It has no separate release policy.

The tag comes from that metadata. Changed attempts, changed metadata, missing
tests and incomplete verification refuse selection. The existing shared
publisher then checks source, attestations and file bytes before it creates a
release. A package tag never changes the native runtime's latest release.

Use [Release a source package](docs/release.md) for the manual build and
promotion steps. The [generated API reference](docs/api.md) describes the build
and selection interfaces. The GitHub workflow adapter selects Smart Ship as
its first package. These workflows do not install a shipping controller or
grant it authority over a repository.
