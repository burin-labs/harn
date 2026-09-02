# Run and Session View Fixtures

This corpus pins the public `harn.run_view.v1` and `harn.session_view.v1`
projection boundary. The `records/` files are sanitized run-record inputs; the
`expected/` files are deterministic projection snapshots generated from the
shared `harn-vm` builder. The corpus includes execution identity, ordered spans,
public flight-recording metadata, explicit evidence gaps, and a local artifact
path that must be absent from public projections.

Run the drift check with:

```sh
make check-run-view-fixtures
```

Refresh snapshots after an intentional view contract change with:

```sh
make gen-run-view-fixtures
```

Downstream clients may vendor this directory or run the same check target to
detect projection drift without depending on portal internals or private
run-record fields.
