- Binary-size growth no longer blocks a release. The distribution fuse remains
  fail-closed and is now the only size level that can refuse a build; a growth
  crossing is reported as a warning wherever it is measured. A stale baseline
  therefore costs a reader's attention rather than a release, which is what a
  baseline that had not been refreshed since 0.10.118 cost the v0.10.126 cut.
- Every Rust pull request now gets a binary-size comment: the debug `harn`
  binary CI already builds, compared against main's last measurement of the
  same, alongside main's most recent release-profile measurement and its
  remaining headroom under the distribution fuse. The check adds no build and
  can never fail a pull request. A missing measurement is reported as missing
  rather than rendered as no growth.
- The release binary-size report is now published as a source-qualified
  artifact with a machine-readable `binary-size.json` beside it, so the release
  orchestrator can tell whether an exact commit has already been measured
  instead of rebuilding it.
