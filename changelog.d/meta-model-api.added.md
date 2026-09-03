- **The Meta Model API is in the provider catalog, and a model row can now
  declare its own training posture.** Meta's Muse Spark routes are served over
  the existing OpenAI-compatible wire, so no adapter was added. Meta publishes
  each generation twice: `muse-spark-1.3` at list price, and
  `muse-spark-1.3-contributor` about 12x cheaper on input in exchange for
  permission to train on the traffic. That posture varies *within* one
  provider, which the provider-level `data_controls` declaration cannot
  express, so model rows now carry an optional `data_controls` of their own and
  a row's declaration overrides its provider's.
- **The `strictest_available` data-controls posture now refuses a route that
  trains on API traffic.** Previously it applied every declared per-request
  control, which on such a route is zero controls: the request went out
  unchanged and the receipt read `no_control_available`, so a strict call could
  quietly achieve nothing. It now fails with a message naming the route and
  citing the source behind the claim. This also closes the same gap for
  DeepSeek and Cohere, which the catalog already classified as training on API
  traffic with nothing stopping a strict run from reaching them. The shipped
  `default` posture is unchanged and refuses nothing, and an unresearched
  provider still reports through the receipt rather than failing the call.
