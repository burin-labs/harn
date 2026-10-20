The Linux process sandbox now lets a child re-assert its own user and group ids
(`setuid`, `setreuid`, `setresuid` and the group forms, with every argument `-1`
or an id the process already holds). GNU make does this before each recipe, so
`make` failed under the `worktree` profile with `Operation not permitted`. A call
that would change an id is still refused.
