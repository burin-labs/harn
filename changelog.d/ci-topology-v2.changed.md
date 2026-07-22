- CI: the Behavior build+suite leg now routes to the big-labeled self-hosted
  workers (one per box, with a persistent warm target directory and wider
  compile fan-out), guarded by a new `linux_big` idle-capacity probe signal.
  If the leg fails anywhere — including a box dying mid-run — a hosted
  fallback job reruns the identical build+suite so degraded infrastructure
  costs minutes, never a red build. A dedicated `cli-build` lane now produces
  the small portable Harn CLI bundle, letting the audit lanes start minutes
  into the run instead of waiting out the full behavior compile.
