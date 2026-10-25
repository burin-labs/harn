The shell guard now reads git aliases from the effective git configuration, so `git s` with a
repository-configured `s=stash pop` is refused like `git stash pop`.
