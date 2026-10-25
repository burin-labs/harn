The agent shell guard now refuses `git stash` writes launched through `xargs`, such as
`xargs -n 1 git stash drop`, and through an inline git alias such as
`git -c alias.s=stash s pop`. The `xargs` form is read from the command scanner
instead of a second option table.
