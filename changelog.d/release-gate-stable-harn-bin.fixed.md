Release audit now runs Harn/script lanes through a stable copied `harn` binary
instead of Cargo's relinked target path, avoiding `(deleted)` self-spawn
failures while Rust audit lanes rebuild in parallel.
