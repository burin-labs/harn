The agent shell guard now refuses `git stash` writes launched through `xargs`, such as
`xargs -n 1 git stash drop`. It reads the launched command from the command
scanner instead of keeping a second option table.
