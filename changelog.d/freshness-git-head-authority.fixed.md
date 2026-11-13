Harn's binary freshness check no longer rejects a checkout's own Git `HEAD` as a conflicting input
when the Cargo build also watches it for the embedded source revision. The Git `HEAD` authority,
which already covers the exact file bytes, now governs that path.
