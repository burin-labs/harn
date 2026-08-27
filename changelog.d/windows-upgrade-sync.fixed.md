Windows upgrades now durably flush staged binaries through a write-capable
file handle before replacing the installed executable. Durable batch state now
uses the shared cross-platform compare-and-replace contract, and native Windows
CI exercises the release-critical Harn CLI test set before merge.
