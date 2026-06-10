The Windows CI red in the toolchain-PATH normalizer is fixed: the unit tests
now build their PATH fixtures and expectations with `std::env::join_paths`
(platform separator) instead of hardcoding a unix `:`, so the
prepended-toolchain assertions pass on Windows (`;`) as well as unix (`:`).
Implementation behavior was already correct on Windows; only the test fixtures
were unix-specific.
