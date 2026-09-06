The shared development cache for per-worktree Cargo target directories is now
actually collected. Its garbage collector ran only from setup profiles that a
worktree never uses, and stamped its interval per worktree, so on a machine
carrying 80 worktrees no sweep had run and entries survived for weeks. Every
profile now runs it, the interval is stamped beside the cache so one worktree
per day pays the sweep rather than each one, and the entry belonging to the
worktree being set up is never collected by its own setup run.
