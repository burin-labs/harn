The shared shell guard no longer prefers a cargo `debug` build when a
release-grade interpreter is available. A debug artifact starts slower and is the
file cargo rewrites mid-build, which tied hook latency to whatever was being
compiled; it is now the last resort rather than the first choice. `HARN_BIN`
still overrides the choice outright.
