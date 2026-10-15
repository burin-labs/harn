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

The [generated API reference](docs/api.md) describes the build request and result.
The GitHub workflow adapter selects Smart Ship as its first package.
<!-- gap: burin-labs/harn#9947 -->
Candidate and promotion workflow wiring remains in progress under
[the package delivery issue](https://github.com/burin-labs/harn/issues/9947).
