Host hardware detection now has a single owner. `harn doctor` and `harn quickstart` each carried their own RAM, GPU,
and free-disk probes alongside the ones in `commands/hardware.rs` — three independent implementations that disagreed
on both source and semantics. Both now read the shared snapshot, so `harn doctor`, `harn quickstart`, `harn local *`,
and `harn models recommend` cannot drift apart. Total RAM and free disk come from `sysinfo` instead of shelling out to
`sysctl`, `df`, and `/proc/meminfo`; the `vm_stat`/`/proc/meminfo`/`df` text parsing and their `#[cfg(target_os)]`
arms are gone.

Free disk is now `sysinfo`'s "space you could realistically write" figure rather than the raw free-block count
(`f_bfree`, which `fs2::free_space` returned), and THE PRINTED NUMBER CHANGES. On macOS `sysinfo` returns
`AvailableCapacityForImportantUsage`, which counts purgeable caches and local snapshots the OS reclaims under
pressure — so `harn doctor` free disk reads HIGHER than before (measured 308 GB -> 325 GB on one host), matching what
Finder's "Available" shows. On Linux/BSD it is `f_bavail`, which excludes root-reserved blocks, so there it reads
slightly LOWER than the raw free count. Both are the space a normal writer actually gets, which is what `df` and the
doctor output already claim to report. The `harn doctor` hardware check still warns under 5 GB and fails under 1 GB;
on macOS the higher figure makes it marginally less likely to warn on a nearly-full disk, which is correct because
that purgeable space is genuinely reclaimable.

Available RAM on macOS deliberately keeps its `vm_stat` reclaimable-pages estimate rather than moving to
`sysinfo::available_memory()`. That figure is `active + inactive + free` — pages processes are currently resident in,
not pages reclaimable under pressure — and measured ~2x higher on a loaded 48 GiB machine (32.5 vs 16.5 GiB). Both
`harn local launch` (its OOM gate) and `harn models recommend` (its RAM bucketing) treat this number as a headroom
budget, so the inflated value would silently disable both guards. Linux available RAM does move to `sysinfo`, where it
is `MemAvailable` and byte-identical to the previous parse.
