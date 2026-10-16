# October 16, 2026 portable-option evidence

The [hosted option campaign](https://github.com/burin-labs/harn/actions/runs/38863445588)
measured 39 catalog mismatches on 20 newly cataloged OpenRouter routes.
The correction enables 14 accepted `top_k` options and disables 25 rejected
options. It leaves three cells without usable measurements unchanged.
This reference records evidence for catalog review #10021 and recovery #9039.

## Source and retained artifacts

| Identity | Value |
| --- | --- |
| Source revision | `34b71cb931be2970a81a7836223c65bb6e1838ab` |
| Harn version | `0.10.167` |
| Catalog SHA-256 | `a3206b9f83549a2800840c19e240890923e15991cfbb66e9c5e2e7a062d301d6` |
| GitHub artifact | `12030912399` |
| Artifact SHA-256 | `37c82a34486b6fd91cf0b4e7f6b349e0770cb00e07d1d173e1f1ff92119f0cf2` |
| Report SHA-256 | `83f0655807570ba8b75fee0dc3683e2d15e220f0a5fb2824ac48446bf07f2e17` |

The report selected 70 routes and 490 option cells from 399 catalog models.
All selected routes received credentials: Anthropic 2, Fireworks 4, and
OpenRouter 64. No credential names were missing.

Verdicts were 427 pass, 39 fail, 2 error, 21 skipped for account policy,
1 unavailable, and 0 pending. The measurement census closes:
490 = 378 measured-supported + 88 measured-unsupported + 21 account-policy
exclusions + 3 eligible cells without measurements. The 39 mismatches are
included in the two measured directions; they aren't additional cells.

The report accounts for 581 physical requests, including one retry, with
578 priced and 3 unpriced requests. Reported priced spend was $0.00708830217.
Its $0.15 fallback reserve brought the budget charge to $0.15708830217,
below the existing $5 ceiling. `actual_usd_known` remains false. Mistral
requests routed through OpenRouter's restricted BYOK integration incur a
separate direct-provider bill even when OpenRouter reports zero cost.

## Measured corrections

Every accepted request used `provider.require_parameters: true`, returned
nonzero input and output usage, and named the requested model in its response.
Every rejected request returned the parameter-routing 404 and had a successful
same-model control without the tested option. These controls distinguish
unsupported options from unavailable models and silently discarded parameters.
They establish endpoint acceptance, not generation quality or reproducibility.

The extracted [39 observations](../../../crates/harn-provider-catalog/src/catalog/portable_option_observations.json)
retain each model, option, verdict, physical request count, selected model,
usage, and individual artifact path. The owning catalog test checks all 39
against resolved capabilities and asserts both directions and the exact census.

Fourteen exact OpenRouter routes gain `top_k`: Granite 4.2 8B, Ling 3.1 Flash,
LFM 2.5 24B, Glimmer 30B, Nex N2.5 Pro, Nemotron 3.5 Lightning and Super VL,
Perceptron Mk1.5, Qwen 3.8 Flash and Omni Flash, Step 5 Preview, Hy4 Preview,
Inkling, and Inkling Small. Exact wire IDs are retained in the observations.

| Exact OpenRouter model | Options withdrawn |
| --- | --- |
| `aion-labs/aion-3.5-mini` | stop, presence penalty, frequency penalty, seed |
| `bytedance-seed/seed-2-1-turbo` | presence penalty, seed |
| `inception/mercury-2.5` | presence penalty, frequency penalty, top_p, seed |
| `liquid/lfm-2.5-24b-a2b` | top_p |
| `moondream/moondream3.1-9b-a2b` | stop, presence penalty, frequency penalty, seed |
| `nex-agi/nex-n2.5-pro` | stop, presence penalty, frequency penalty, seed |
| `perceptron/perceptron-mk1.5` | stop, seed |
| `upstage/solar-mini4` | stop, seed |
| `upstage/solar-pro4` | stop, seed |

The existing OpenRouter option overlays own these changes. Their `extends`
rules preserve tool, reasoning, vision, and other family facts. Exact model
matches keep direct-provider routes and free variants outside these receipts.
Catalog, matrix, and support projections are regenerated from those sources.

## Remaining coverage limits

- Glimmer 30B `seed` returned 429 on its initial request and retry. It remains
  unmeasured; rate limiting doesn't justify withdrawing support.
- `openrouter/free` frequency penalty returned a routing 404, but its control
  returned 429. The failed control leaves the cell unmeasured.
- `openrouter/free` `top_k` returned a routing 404 while its control selected
  Cohere North Mini Code Free. A changing pool's temporary lack of a compatible
  endpoint is an unavailable outcome, not stable unsupported-option evidence.
- Twenty-one cells carry explicit account-policy exclusions. Privacy settings
  weren't relaxed, and these refusals don't count as support measurements.

Issue #9039 remains open until an exact-source hosted campaign measures every
eligible cell. The catalog correction resolves the 39 measured mismatches;
it doesn't turn incomplete provider coverage into a passing campaign.
