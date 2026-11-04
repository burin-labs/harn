The harn-target collector now removes idle entries on Linux hosts. It reads
`/proc` and Cargo's build locks instead of an unprivileged `lsof`, which could
never read root's processes and so kept every entry.
