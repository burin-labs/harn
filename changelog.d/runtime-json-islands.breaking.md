Migration: Rust callers exhaustively matching `WireShape` must handle the
runtime-only `RuntimeJson` variant. Runtime preparation retains its compiled
catalog schema validator. Portable graph construction and serialization reject
this variant; the portable wire protocol is unchanged.
