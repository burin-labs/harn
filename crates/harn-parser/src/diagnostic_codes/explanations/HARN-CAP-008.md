# HARN-CAP-008 — declared host capability operation is not served

Harn found a declared host operation that the target host does not serve.
`harn check` reads the file named by `host_served_capabilities_path`. ACP checks
the operations advertised by the connected host when a prompt starts.

Add the operation to the host, remove the declaration, or list its exact name
in `runtime_installed_host_operations` if its handler is added at runtime.
