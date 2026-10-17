Code-index snapshot restores publish the reconciled index before rewriting
the snapshot in the background. Later restores, rebuilds, and explicit writes
on the same capability wait for earlier writes before starting.
`CodeIndexCapability::wait_until_idle` waits for background builds and writes;
embedders should call it before exiting after a restore.
