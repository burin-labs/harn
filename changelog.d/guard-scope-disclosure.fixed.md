Repository guards that derive their scope from a glob now fail when the glob
matched nothing, instead of reporting a clean sweep. `check_ci_cache_policy`,
`check_rust_test_lane_policy`, and `check_receipt_struct_duplication` each drew
their entire finding set from a file walk; if that walk matched nothing — wrong
invocation root, a renamed directory — every policy held vacuously and the check
exited 0. `check_receipt_struct_duplication` printed nothing at all, so a scan of
2,621 files and a scan of none looked identical. Each now prints what it
inspected and treats an empty scope as a failure, with the message naming the
pattern that matched nothing.
