Linux subprocesses launched with `--allow-process-network` can resolve hostnames
when `/etc/resolv.conf` points outside `/etc`. The Landlock profile now grants
the canonical inode for exact name-service files without opening the broader
`/run` tree.
