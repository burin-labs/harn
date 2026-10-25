Release archives for macOS and Linux are now `.tar.xz`, about 42% smaller than `.tar.gz`.
`install.sh`, `harn upgrade`, the runtime installer, and the container image read them.
`install.sh` needs `xz` on Linux; macOS `tar` unpacks it natively.
Each release also carries a `.tar.gz` copy of the four macOS and Linux archives, used only
by the ACP registry manifest because the registry cannot unpack `.tar.xz`.
