Every push to the main branch now gets its own CI run, and keeps it. Main
pushes previously shared one concurrency key and cancelled on push, so a merge
could evict a pending run before any job started and kill one already
executing. Merges land closer together than a run takes, so most main commits
never finished being proved, and a commit whose run was discarded carries no
executed result at all, which reads the same as a passing one. Main runs are
now keyed per commit and are never superseded, so they execute in parallel.
Pull-request runs still supersede when a new commit arrives.
