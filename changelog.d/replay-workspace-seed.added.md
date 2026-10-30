Hosts can capture a bounded private workspace seed and restore repeated trials at one stable path through
`workspace_checkpoint::ReplayWorkspace`. Captures retain ignored files, executable bits, and relative links
inside the workspace, and refuse external links, changed seeds, and resource-limit overruns. Offline coding
replay uses this shared owner and includes the measured seed identity in each trial's evidence.
