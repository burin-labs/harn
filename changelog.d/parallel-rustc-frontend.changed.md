Local setup and CI now compile Harn's own crates with rustc's parallel front
end (`-Zthreads=8`). Rebuilding `harn` after a `harn-vm` edit dropped from 90s
to 55s, because `harn-vm`'s own compile went from 58s to 32s. The flag is
unstable, so a workspace rustc wrapper passes it with `RUSTC_BOOTSTRAP=1` to
workspace crates only, and dependencies compile exactly as on stable. Set
`HARN_RUSTC_FRONTEND_THREADS=1` and rerun `make setup` to turn it off.
