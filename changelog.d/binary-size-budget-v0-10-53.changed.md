Raised the x86_64 Linux release binary-size ceiling to 224 MiB and rebaselined
it on the v0.10.53 candidate (233,410,816 bytes at `14aad89`). The previous
220 MiB ceiling was set against the v0.10.51 baseline and left 1.78 MiB of
headroom, which cumulative growth since then consumed.
