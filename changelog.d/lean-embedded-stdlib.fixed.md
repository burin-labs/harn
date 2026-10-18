An embedder that links a lean `harn-vm` can now carry the precompiled stdlib.
`harn-stdlib-artifacts` no longer forces the full VM: select the same builtin
families your `harn-vm` resolves to, for example
`features = ["embed", "sqlite", "native-keyring"]`. `register()` now refuses a
table compiled for another builtin contract instead of accepting one that
every import would miss.
