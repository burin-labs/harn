# October 26, 2026 portable-option evidence

The [hosted option campaign](https://github.com/burin-labs/harn/actions/runs/40315922543)
reported 13 catalog mismatches on six OpenRouter routes. Three requests accepted
`top_k`; ten rejected options returned a parameter-routing 404 while the same
model accepted a control without that option. The catalog correction applies
only to those exact routes and options.

| Identity | Value |
| --- | --- |
| Source revision | `72e20ecb528bf38aace65865565dfc811f07ceed` |
| Harn version | `0.10.181` |
| Catalog SHA-256 | `92bc6fdaa6f07eb44a6b4e40222f0d9e8983715f467377a30cdd71d0d4b5881e` |
| GitHub artifact | `12647567458` |
| Report SHA-256 | `bbed45ecd7e92e419d0c31487de803eed6214005fc2159a2826b59522c00aff2` |

The report selected 500 cells: 452 passed, 13 failed, 13 errored, 21 were
excluded by account policy, and one was unavailable. The campaign's control
census was incomplete because of the unmeasured and unavailable cells. The
correction does not turn those cells into evidence or claim that the provider's
behavior remains unchanged today.

The [13 observations](../../../crates/harn-provider-catalog/src/catalog/portable_option_observations_20261026.json)
retain the selected model, option, verdict, physical request count, and usage.
The catalog test checks both measured directions against resolved capabilities.
The accepted calls used `provider.require_parameters: true`, returned usage,
and named the requested model. The rejected calls each include a successful
same-model control. These observations establish parameter routing at the time
of the probe, not generation quality.

Refs #10634
