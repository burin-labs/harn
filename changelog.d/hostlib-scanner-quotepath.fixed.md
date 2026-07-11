- Keep non-ASCII tracked-file and churn paths literal in the git-backed
  scanner by passing `core.quotepath=false` (and NUL-delimiting `ls-files`),
  so paths like `src/café.rs` match their real on-disk names.
