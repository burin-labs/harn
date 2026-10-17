Claude Code cloud sessions now build and test within the cloud VM's memory and
disk: Cargo runs at CI's compile budget with incremental state and debuginfo
off, an 8 GB swap file absorbs two large compiles at once, and the cloud warm
installs CI's pinned cargo-nextest so `make test` runs.
