# Parallel test-case performance check

`make check-test-case-performance` detects repeated per-test setup work that
functional tests and the VM RSS soak cannot see. It runs four isolated Harn
test processes concurrently. Each process executes 16 deterministic passing
tests with `--parallel --jobs 4 --diagnose`; every suite contains a real package
generation, an 8 KiB lockfile, a `std/` import, and a relative import.

The controller wraps each child in the platform `/usr/bin/time`, requires all
64 machine-readable diagnostic lines, and rejects failed, timed-out, malformed,
or incomplete samples. Checked metrics are maximum child wall time, summed user
and system CPU, and setup/execute percentiles. RSS, page faults, context
switches, and filesystem I/O are telemetry-only because cache state makes them
too variable for a release-blocking assertion.

Baselines in `baselines.toml` are measured observations. Each checked limit is
the larger of twice its observation or a small additive noise floor. A warning
starts at 80% utilization. Do not add a platform row by copying another
platform or by guessing: run the check on that platform, inspect its emitted
JSON, record the exact successful observation and source commit, then rerun it
to prove the row passes. A missing row intentionally fails closed while still
printing the calibration payload.

The workload specifically protects the harn#4815 failure mode: package
snapshot acquisition accidentally moved ahead of cheap `std/` and relative
import resolution. The workload keeps a valid package snapshot available so
unnecessary probes do real lock, TOML, read, and SHA-256 work instead of taking
an empty fast path.
