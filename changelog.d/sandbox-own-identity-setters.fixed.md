The Linux process sandbox now lets a child re-assert its own user and group ids:
`setreuid`, `setresuid` and the group forms are admitted when every argument is
`-1` or the value that same slot (real, effective, saved) already holds, and
`setuid` and `setgid` when those slots already agree. GNU make does this before
each recipe, so `make` failed under the `worktree` profile with `Operation not
permitted`. A call that would move any slot to a different id is still refused.
