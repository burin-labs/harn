Generated command drivers in the system temporary directory now keep run records in a per-user store.
Privileged invocations no longer block ordinary users from running commands such as `harn precompile`.
The shared-temp user namespace is private and rejects unsafe existing directories before record writes.
