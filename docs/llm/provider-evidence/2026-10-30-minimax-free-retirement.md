# October 30, 2026 MiniMax free-route retirement

The [October 26 hosted option campaign](https://github.com/burin-labs/harn/actions/runs/40315922543)
received an explicit OpenRouter retirement response for
`minimax/minimax-m3.1-flash:free`: its free launch ended October 22, with the
paid `minimax/minimax-m3.1-flash` route named as the replacement. All seven
selected option cells for the free route were unmeasured because the route
was retired. They are not capability failures or successful controls.

The [public model catalog](https://openrouter.ai/api/v1/models), read without
credentials or inference on October 30, contained 456 models. The retired
free route was absent; the paid replacement, `minimax/minimax-m2`, and
`qwen/qwen3-coder-next` were present. The response SHA-256 was
`efcd281a16357f8c93643dcc08aa27b060027b994834ccb220bc376fd1ffdefa`.

The retained campaign report has SHA-256
`bbed45ecd7e92e419d0c31487de803eed6214005fc2159a2826b59522c00aff2`
and GitHub artifact ID `12647567458`. Its five MiniMax M2 cells reported a
generic unavailable-model response, and one Qwen cell reported a rate limit.
Those observations do not establish retirement; both routes remain active.

Only the explicitly retired free route receives a structured sunset and
deprecation. The paid OpenRouter replacement is not a registered Harn model,
so this change adds no replacement pointer or new route. The separate 13
catalog contradictions in the same campaign were corrected in
[the October 26 evidence](2026-10-26-portable-options.md).

An offline check loaded the regenerated provider configuration through the
published 0.10.182 CLI and passed its catalog to the existing campaign planner.
The released baseline selected the retired route once; the corrected source
selected it zero times. Both selections retained MiniMax M2 and Qwen Coder Next,
with 373 and 372 total planned routes respectively. No inference requests ran.
