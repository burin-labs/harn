`harn fix` takes `--code <CODE>` (repeatable) to plan or apply only the repairs
for named diagnostics. A targeted migration no longer has to accept every other
repair that shares its safety class. The selector narrows the plan itself, so
`--plan --code X` shows exactly what `--apply --code X` writes.

`--apply` also preserves a file's formatting state. A repair changes line
lengths, so a shortening rename could leave a canonically formatted package
failing the `harn fmt --check` its own CI runs. An edited file that was already
canonical is now formatted again; one that was not is returned exactly as its
author keeps it, rather than arriving as a whole-file diff.
