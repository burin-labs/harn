Ad hoc Makefile Harn CLI fallbacks now isolate Cargo's intermediate build
directory under the active target directory, avoiding stale shared build
artifacts when generated-artifact checks source-build the CLI.
