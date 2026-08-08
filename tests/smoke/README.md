# harn-release-smoke

Cross-platform release smoke fixtures exercised by
[`scripts/release_smoke.harn`](../../scripts/release_smoke.harn) and the
[`release-smoke` CI matrix](../../.github/workflows/release-smoke.yml).

The package itself is intentionally minimal. The smoke driver checks
the manifest and exported symbols on every supported platform so a
release tag never publishes a binary that breaks the user-visible
package surface on macOS, Linux, or Windows.

See
[`docs/src/dev/platform-compatibility.md`](../../docs/src/dev/platform-compatibility.md)
for the per-capability support matrix.
