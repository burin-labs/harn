The agent-gate census that checks every declared `agent_loop` option key is
inventoried now asks the whole registry, through the registry's own list of
entry files, instead of two filenames repeated inside the check.

Entry names share one flat namespace and the registry refuses duplicates, so a
declared key is filed under whichever entry file owns its theme. Asking only
two of those files made a correctly filed key indistinguishable from a missing
one, and made a newly declared key impossible to satisfy: filing it in the
model file duplicated the existing entry and the audit threw.

The reverse direction still asks only the files that own the model shape, so an
entry naming a key the shape no longer declares is still reported as stale. A
failure now names the offending key rather than reporting every element of a
shifted sorted list.
