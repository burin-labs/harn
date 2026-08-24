# HARN-CAP-008 — declared host capability operation is not served

`harn check` found an operation in the project's declared host operations but
not in the file named by `host_served_capabilities_path`.

Add the operation to the host, remove the declaration, or list its exact name
in `runtime_installed_host_operations` if its handler is added at runtime.
