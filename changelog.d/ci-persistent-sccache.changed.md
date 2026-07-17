Compatible heavy Linux CI lanes now use kill-switch Blacksmith runners and
lane-local persistent sccache disks, with cache hit/miss statistics captured
before server shutdown. Rust tests and Harn conformance stay on GitHub Ubuntu
so sandbox and host-tool behavior proofs retain their established environment.
