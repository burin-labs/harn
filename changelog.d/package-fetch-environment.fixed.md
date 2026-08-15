A package fetch that fails now says which environment it ran in. Harn clones
packages with an isolated Git environment that offers no credential helper, no
SSH agent and no user or system Git config, so Git's own advice to "make sure
you have the correct access rights" pointed readers at credentials that were
never offered and never will be. The failure now names that constraint. Package
materialization also gained a regression test proving that a run served by an
already-materialized generation makes no fetch attempt at all, so a project can
keep running against a source it can no longer reach.
