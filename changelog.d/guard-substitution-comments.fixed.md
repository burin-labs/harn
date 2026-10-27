The shared shell guard now inspects executable substitution stages even when shell comments contain delimiter
characters, so it refuses stash mutations without rejecting read-only substitutions.
