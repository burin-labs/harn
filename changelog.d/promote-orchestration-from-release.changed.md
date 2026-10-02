- A release's `repin` job now starts harn-bump-fleet's orchestration promotion instead of dispatching
  fleet-owned bump adapters directly, so consumers bump only after their adapter runs the released
  driver. The release candidate gate checks the driver that promotion will select, and warns when the
  older current pin cannot drive the candidate.
