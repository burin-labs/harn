# Deploy to Fly.io

Harn ships a Fly template at `deploy/fly/fly.toml` and the deploy helper can
generate a project-local app config:

```bash
harn orchestrator deploy \
  --provider fly \
  --manifest ./harn.toml \
  --name harn-prod \
  --region sjc \
  --image ghcr.io/acme/harn-prod:latest \
  --build
```

Before the first deploy, create the app and its persistent volume:

```bash
fly apps create harn-prod
fly volumes create harn_data --app harn-prod --size 10 --region sjc
```

The generated `fly.toml` keeps one machine running by default so cron-heavy
workloads do not miss scheduled fires during scale-to-zero cold starts. It
uses `/healthz` for HTTP checks and exposes Harn's Prometheus metrics from
`/metrics` on the same internal listener port.

Secret sync uses `fly secrets set`. The deploy helper syncs values supplied
with `--secret KEY=VALUE`, common provider keys such as `OPENAI_API_KEY`, and
env-backed manifest secrets like `HARN_SECRET_GITHUB_WEBHOOK_SECRET` when
those variables are already present locally.

Fly provides automatic TLS on the public hostname. Keep the orchestrator
container on plain HTTP with `HARN_ORCHESTRATOR_LISTEN=0.0.0.0:8080`.
