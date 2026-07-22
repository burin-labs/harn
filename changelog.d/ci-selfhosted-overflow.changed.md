- CI: heavy Linux lanes (package audit, Rust lint, Rust test, audit scripts)
  now route to idle self-hosted runners when available, overflowing to hosted
  runners whenever the pool is busy, offline, or the workflow run is a fork PR
  (which never receives the probe credentials). The Behavior build stays on
  hosted runners because it uploads the multi-gigabyte behavior payload. Kill
  switch: `HARN_CI_DISABLE_SELFHOSTED_LINUX=true`.
