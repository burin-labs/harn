- Write local dependency paths into `harn.toml` with POSIX separators so a
  manifest authored on Windows stays portable to Unix checkouts instead of
  embedding backslashes that fail to resolve.
