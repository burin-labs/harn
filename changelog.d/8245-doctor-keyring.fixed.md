`harn doctor` no longer hangs on a Linux host whose login keyring is locked.
The credential-store health check now gives up after five seconds and reports
the store as unavailable because it is waiting on an interactive unlock, rather
than blocking forever on a prompt that a headless host can never answer.
