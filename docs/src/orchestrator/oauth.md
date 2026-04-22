# Connector OAuth

`harn connect` is the guided setup entry point for connector credentials. It is
intended for local operator setup: run the browser flow once, store tokens in
the workspace keyring namespace, then reference the resulting secret ids from
connector configuration.

## Provider flows

Built-in OAuth commands are available for Slack, Linear, and Notion:

```bash
harn connect slack \
  --client-id "$SLACK_CLIENT_ID" \
  --client-secret "$SLACK_CLIENT_SECRET" \
  --scope "app_mentions:read chat:write"
harn connect linear \
  --client-id "$LINEAR_CLIENT_ID" \
  --client-secret "$LINEAR_CLIENT_SECRET"
harn connect notion \
  --client-id "$NOTION_CLIENT_ID" \
  --client-secret "$NOTION_CLIENT_SECRET"
```

The generic OAuth 2.1 path targets compliant protected resources:

```bash
harn connect generic acme https://mcp.example.com/mcp
harn connect --generic acme https://mcp.example.com/mcp
```

When endpoints are not supplied, Harn discovers protected-resource metadata,
then authorization-server metadata. If the server advertises dynamic client
registration and no `--client-id` is supplied, Harn registers a loopback client
for the selected redirect URI.

GitHub App setup is separate from OAuth access tokens:

```bash
harn connect github \
  --app-slug my-harn-app \
  --app-id 12345 \
  --private-key-file app.pem
```

The command opens the GitHub App installation URL and waits for the app setup
callback to include `installation_id`. You can skip the browser callback when
you already know the installation:

```bash
harn connect github \
  --installation-id 67890 \
  --app-id 12345 \
  --private-key-file app.pem
```

## Callback server

OAuth and GitHub App setup callbacks bind only to `127.0.0.1` or `localhost`.
The default redirect URI uses port `0`, so Harn chooses a random free local port
and sends that concrete URI in the authorization request. Callback listeners are
single-use and time out after five minutes. If a callback request includes an
`Origin` header, Harn requires it to match the redirect origin.

## OAuth guarantees

Harn always sends PKCE S256 parameters. The generic flow validates advertised
PKCE support when authorization-server metadata is available. The generic flow
also sends the `resource` parameter to both the authorization endpoint and token
endpoint; provider-specific flows let you override `--resource` when a provider
requires one.

## Stored secrets

OAuth setup stores connector-friendly ids in the same keyring namespace that
the nearest `harn.toml` uses:

- `<provider>/access-token`
- `<provider>/refresh-token` when returned by the provider
- `<provider>/oauth-token` for refresh metadata such as token endpoint, client
  id, client secret, scopes, resource, and expiration

GitHub App setup stores:

- `github/installation-<id>`
- `github/app-<app-id>/private-key` when `--private-key-file` is supplied
- `github/webhook-secret` when a webhook secret is supplied

Use `harn connect --list`, `harn connect --refresh <provider>`, and
`harn connect --revoke <provider>` to inspect, refresh, or remove local
connector credentials.
