- `build-release-binaries.yml` now records, at each of the three steps that
  harn-bump-fleet's pre-tag gate matches by display name, that renaming them is
  a cross-repo change. The Actions API exposes no stable step id, so the gate
  has to match on the name; #6226 renamed one and the next release spent 43
  minutes before reporting a step that had run and passed as unexecuted.
