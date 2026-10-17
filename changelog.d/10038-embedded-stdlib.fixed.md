Compile embedded standard-library artifacts with the same full native builtin
contract as the distributed runtime, so cold imports load the shipped artifacts
instead of rejecting them and recompiling the standard library.
