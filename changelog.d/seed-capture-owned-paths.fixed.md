A workspace seed capture no longer fails with "workspace changed while capturing its seed" when
Harn's own runtime state, such as `.harn/` stores and `.harn-runs/`, is written during the capture.
`ReplayWorkspace::capture_excluding` also skips paths the host names as its own state.
