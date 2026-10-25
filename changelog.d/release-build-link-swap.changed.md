Linux release builds add a 16 GB swap file before building when the runner has less than 12 GB of swap,
so the fat-LTO link of `harn` survives its memory peak on the 16 GB ARM64 runner.
