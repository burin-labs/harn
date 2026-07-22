- CI: heavy Linux lanes (package audit, Rust lint, audit scripts) now route to
  idle self-hosted runners when available, overflowing to hosted runners when
  the probe observes the pool busy or offline, or the workflow is a fork PR
  (which never receives the probe credentials). The availability check is a
  snapshot rather than a reservation. The behavior-payload lanes (Behavior
  build, Rust test) stay on hosted runners because Actions artifact storage
  throughput to the boxes cannot move the multi-gigabyte payload quickly. Kill
  switch: `HARN_CI_DISABLE_SELFHOSTED_LINUX=true`.
