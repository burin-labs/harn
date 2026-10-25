The shell guard now analyzes the bodies of `$(...)`, backticks and process substitution as command lines,
so `echo $(git stash pop)` is refused like the bare command. A body that never closes is refused.
