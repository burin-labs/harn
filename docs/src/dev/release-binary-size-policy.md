# Release binary-size policy

`.github/release-binary-size-policy.json` is the source of truth for release
binary-size admission. Each target entry binds an exact measured candidate to
an integer byte ceiling. The policy keeps at least 1 MiB and at most 4 MiB of
headroom above that measurement.

The x86_64 Linux gate uses a stripped release binary. When it fails, record the
candidate version, source SHA, byte count, and observation time from the failed
Actions run. Raise the ceiling only enough to restore the bounded headroom.
Do not estimate from a local binary built for another target.

`.github/release-binary-size-policy.jq` is the bootstrap validator used before
the release job installs or builds Harn. `scripts/check_binary_size.harn`
decodes the same closed contract for local checks. The focused PR gate exercises
both validators against unknown fields, invalid metadata, duplicate targets,
topology failures, and both headroom bounds.

The release workflow resolves the policy before starting its build matrix and
passes the exact byte count to the Linux gate. `--budget-mb`,
`BINARY_SIZE_BUDGET_MB`, and `BINARY_SIZE_BUDGET_BYTES` are explicit local or
recovery overrides; the checked-in policy remains the default.

Only x86_64 Linux is budgeted today. Adding another target entry is data
preparation, not activation: also add the matching workflow gate and report
surface before treating that target as enforced.
