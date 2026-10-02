Provider catalogs can declare `credential_env`: the environment names a
platform credential chain reads besides the provider's key. Bedrock now
declares its AWS credential variables (`AWS_ACCESS_KEY_ID`,
`AWS_SECRET_ACCESS_KEY`, `AWS_SESSION_TOKEN`, `AWS_SECURITY_TOKEN`,
`AWS_PROFILE`, `AWS_CONTAINER_AUTHORIZATION_TOKEN`). Like `auth_env`, these
names are withheld from every spawned child under the `inherited` policy and
can never be allowlisted. Catalog validation now rejects a provider that
requires auth but declares no credential names. Before this, Bedrock passed
through an exemption for its auth style.

Migration: under the `inherited` policy, a command that relied on inheriting
`AWS_PROFILE` or AWS keys must now pass them explicitly in its own `env`.
