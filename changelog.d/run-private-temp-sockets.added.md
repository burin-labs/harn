Hosts can grant Unix-domain sockets in one Harn execution's private temporary
directory. Concurrent executions in the same workspace receive separate socket
roots, while their own child processes share one root. Missing execution
ownership refuses command preparation instead of using shared temporary storage.
