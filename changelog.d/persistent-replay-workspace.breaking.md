Replay workspaces can now retain their captured seed across owner restarts.
`ReplayWorkspace::persist` returns a serializable descriptor, and
`restore_saved` acquires an exclusive lease and restores the verified starting
tree before returning. Competing owners, changed seeds, and nonprivate saved
roots refuse. Interrupted replacements recover through the same restore owner.

Migration: Rust hosts inspecting `SeedReceipt.schema` now match
`SeedSchema::V1`; the JSON value remains `harn.workspace-seed.v1`. Exhaustive
`SeedError` matches must handle `InUse`. Keep a workspace handle for the entire
execution and child-process lifetime, and save retained descriptors with the
owning run's private metadata.
