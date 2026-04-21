# Deploy to Render

Harn ships a Render Blueprint at `deploy/render/render.yaml` and the
`harn orchestrator deploy` helper can generate a project-local variant for
your manifest.

```bash
harn orchestrator deploy \
  --provider render \
  --manifest ./harn.toml \
  --image ghcr.io/acme/harn-orchestrator:latest \
  --build \
  --render-service srv-xxxxxxxx
```

The helper validates the manifest by booting the single-tenant orchestrator
runtime in a temporary state directory, writes `deploy/render/render.yaml`,
and writes `deploy/render/Dockerfile`. The Dockerfile packages the current
project on top of the published Harn runtime image so local handlers and
prompt assets are available at `/app` in the container.

Render secrets should live in the `harn-secrets` environment group referenced
by the Blueprint. When `--render-service` is supplied, `--secret KEY=VALUE`
values and locally set Harn secret env vars are also pushed through the Render
CLI before the deploy command runs.

The generated service uses:

- `GET /healthz` for Render health checks.
- `/data` for persistent orchestrator state and the SQLite EventLog.
- `HARN_ORCHESTRATOR_LISTEN=0.0.0.0:8080`.
- `HARN_SECRET_PROVIDERS=env`.

Render provides TLS at the edge, so the orchestrator should run plain HTTP in
the container unless you have a provider-specific reason to terminate TLS
inside Harn.
