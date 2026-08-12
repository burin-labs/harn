- **Release and CI dependency installs tolerate sustained transient download
  failures (#6602).** Immutable dependency downloads now use four total
  attempts with bounded 5s, 20s, and 60s backoff while deterministic failures
  still stop immediately.
