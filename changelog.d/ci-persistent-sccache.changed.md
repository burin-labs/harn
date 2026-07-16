Compatible heavy Linux CI lanes now use kill-switch Blacksmith runners and
lane-local persistent sccache disks, with cache hit/miss statistics captured
before server shutdown. Rust tests stay on Landlock-capable GitHub Ubuntu so
sandbox escape coverage remains real.
