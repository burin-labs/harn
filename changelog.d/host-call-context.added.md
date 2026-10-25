`code_index.rename_symbol` can be stopped while it waits on a language server.
It runs off the VM's thread, so cancelling the tool call returns at once and the
rename writes nothing. A new `deadline_ms` bounds the call and returns `refused`
with `retry_after_ms` and an `alternative` instead of a silent fallback, and the
rename reports typed progress phases on its tool call's update while it waits.
Stopping one command tool call no longer cancels the VM that dispatched it.
Hostlib builtins that write files hold one write lease while they write, so a
rename finishing off the VM's thread never interleaves its writes with another
tool's, and a file another tool changed since the rename read it refuses the
rename instead of being overwritten.
