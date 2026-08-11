- **Windows workspace warm knobs now live in `cache-policy.json`.** Artifact
  name, retention, size budget, nextest pin, and Dev Drive ceilings are owned by
  the typed `windows_workspace_warm` config (schema v3); the pack/restore script
  and CI cache-policy checker both read that document instead of duplicating
  constants.
