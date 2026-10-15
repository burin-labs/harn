Provider configuration parsing, overlay merging, audit contracts, and packaged defaults now compile in
`harn-provider-catalog`. The catalog crate parses its packaged defaults once and shares the immutable snapshot.
Generated provider and capability snapshots now live under `crates/harn-provider-catalog/src/`;
catalog generation and capability-promotion commands use those paths by default.
