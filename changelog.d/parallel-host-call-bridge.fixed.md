A `host_call` inside a `parallel`, `parallel each` or `parallel settle` branch now reaches the
embedder's host. Branches run on runtime worker threads, which did not carry the host-call bridge,
so host operations fell back to built-in defaults or failed as unsupported.
