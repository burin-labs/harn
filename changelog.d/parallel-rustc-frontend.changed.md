Local setup and CI now compile with rustc's parallel front end
(`-Zthreads=8`). Rebuilding `harn` after a `harn-vm` edit dropped from 90s to
55s, because `harn-vm`'s own compile went from 58s to 32s. The flag is
unstable, so the pinned stable compiler accepts it through `RUSTC_BOOTSTRAP=1`.
Set `HARN_RUSTC_FRONTEND_THREADS=1` and rerun `make setup` to turn it off.
