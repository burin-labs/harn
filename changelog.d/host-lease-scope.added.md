- **Harn workflows can hold cancellation-safe machine resource scopes.**
  `std/host_lease::with_host_lease` now owns typed acquire, deferral, and
  release receipts around an arbitrary Harn callback, waits on cross-process
  lease notifications in bounded slices, and binds cleanup to a reusable
  VM-owned `resource_guard` so task cancellation, exceptions, frame teardown,
  and VM drop cannot strand a live-process lease (#4556).
