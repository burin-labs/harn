Run the stdlib script suites together within the caller's existing worker
budget to reuse import compilation and avoid leaving reserved CI workers idle.
New stdlib test files are discovered without hand-listing them in CI.
