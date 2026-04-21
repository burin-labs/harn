# Deploy to Railway

Harn ships a Railway config at `deploy/railway/railway.json`. The deploy
helper can generate it and run the Railway CLI:

```bash
harn orchestrator deploy \
  --provider railway \
  --manifest ./harn.toml \
  --railway-service harn-prod \
  --railway-environment production
```

Railway reads `railway.json` for build and deploy settings. The generated
config uses the Dockerfile builder, starts `harn orchestrator serve`, checks
`/healthz`, and sets the runtime variables needed for a SQLite-backed
orchestrator EventLog.

Secret sync uses `railway variable set`. The helper syncs `--secret KEY=VALUE`
values, common provider API keys from the local environment, and
`HARN_SECRET_*` variables referenced by manifest trigger secrets when they are
set locally. It also stages the public Harn runtime variables and
`RAILWAY_DOCKERFILE_PATH=deploy/railway/Dockerfile` so `railway up` builds the
same deploy bundle it generated. Railway applies variable changes as staged
service changes; review and deploy them in the Railway UI if your project
requires manual approvals.

Railway provides TLS for public domains. Keep Harn listening on plain HTTP in
the container and let Railway terminate HTTPS.
