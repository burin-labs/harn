A language server that the code index stops is now reaped when its session
ends. Before, a server evicted from the session cache stayed a zombie child
until the host process exited, so a long-running host gathered one per
evicted workspace.
