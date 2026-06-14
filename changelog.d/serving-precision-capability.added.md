Added a `serving_precision` provider capability field (`trusted` / `degraded` /
`throttled` / `unverified`) so the capability matrix can label routes that serve
a model at degraded quality or unusable timing. Seeded the known gpt-oss-120b
verdicts (Fireworks + OpenRouter = trusted, SambaNova = degraded/quantized,
Cerebras = throttled) and exposed the field on `harn check --provider-matrix
--json`, giving the Burin meter precision canary a data-driven signal instead of
trusting provider liveness alone.
