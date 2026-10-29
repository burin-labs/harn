The shell guard reaches the same verdict from any working directory. Its Git
alias lookup now gets read access to the caller's repository and to every
repository the command names, instead of only the interpreter's directory tree,
so a `git -C <repo>` command no longer fails the lookup from a subdirectory. A
lookup that still fails is refused as a guard fault that names its cause, not
as a `git stash` refusal.
