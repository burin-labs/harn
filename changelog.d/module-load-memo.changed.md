Module graph builds now parse each module once per process instead of once per
graph, and graphs share each remembered module instead of copying it. A batch
run such as `harn precompile --warm-cache` built several graphs for every entry
and re-parsed every imported module each time, so a large tree lexed hundreds
of times more source than it holds. Cold `--warm-cache` precompiles now use
about a quarter of the CPU time and finish about a third sooner. A remembered
module is reused only when its source text is unchanged and every import it
names still resolves to the same file, so edits, new import targets, and
reinstalled packages are seen as before.
