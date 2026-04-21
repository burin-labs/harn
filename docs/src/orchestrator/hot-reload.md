# Hot reload

Hot reload lets a running orchestrator adopt a new `harn.toml` without
dropping in-flight trigger deliveries.

## Entry points

Use any of these:

- send `SIGHUP` to the orchestrator process
- run `harn orchestrator reload --config harn.toml --state-dir ./.harn/orchestrator`
- `POST /admin/reload` with the same bearer/HMAC auth used by `a2a-push`
- start the orchestrator with `--watch` to reload automatically when the
  manifest file changes

`harn orchestrator reload` discovers the running listener URL from
`orchestrator-state.json` by default. Pass `--admin-url` to target a
different instance explicitly.

## What reload does

Each reload path runs the same sequence:

1. Parse and validate the new manifest.
2. Resolve handlers and predicates in a fresh VM.
3. Prepare connector replacements before touching live bindings.
4. Reconcile manifest trigger ids into added, removed, modified, and
   unchanged bindings.
5. Drain old binding versions, activate new ones, and swap listener
   routes in place.
6. Publish `reload_succeeded` or `reload_failed` on
   `orchestrator.manifest`.

If any step before the final swap fails, the running orchestrator keeps
serving the old manifest.

## Safety model

- In-flight requests keep the binding version they started with.
- New requests route to the newest active binding version.
- Removed bindings stop accepting new work and drain until their
  in-flight count reaches zero.
- Connector reload is staged: the orchestrator initializes and activates
  replacement connectors before swapping them into the live runtime.
- If connector activation or route replacement fails, the orchestrator
  rolls back to the previous manifest/runtime view.

## Auth

`POST /admin/reload` uses the same auth helpers as `a2a-push` routes:

- `Authorization: Bearer <api-key>` where the key comes from
  `HARN_ORCHESTRATOR_API_KEYS`
- `Authorization: HMAC-SHA256 ...` using
  `HARN_ORCHESTRATOR_HMAC_SECRET`

The CLI wrapper prefers the first configured API key and falls back to
the shared HMAC secret when no API key is present.

## Watch mode

`harn orchestrator serve --watch` watches the manifest directory and
debounces reload requests for short bursts of file events. It is useful
for local development and test harnesses; production deployments usually
prefer explicit `SIGHUP` or the admin API.
