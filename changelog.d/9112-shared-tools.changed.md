CI artifacts carry the Harn CLI and its AOT generator from one compilation graph.
Both executables are checked against the source manifest.
Package verification and the release rehearsal reuse this pair instead of compiling separate tool graphs.
