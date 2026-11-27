# November 27 usage-accounting audit

The [remaining accounting audit](https://github.com/burin-labs/harn/issues/7320)
still depends on provider-specific evidence. This review renews the queue
through December 11 and leaves the accounting declarations in place.

- Cloudflare's [compatibility endpoint](https://developers.cloudflare.com/workers-ai/configuration/open-ai-compatibility/)
  and operator-provided local endpoints need observations tied to the configured
  account and upstream. A model-list response does not carry streaming usage
  or cache counters.
- The [Cohere compatibility API](https://docs.cohere.com/v2/docs/compatibility-api)
  documents streamed text, but its examples do not establish the final usage
  frame or cache-counter contract. The native endpoint has a separate schema.
- The [November 15 audit](2026-11-15-usage-accounting.md) records the remaining
  provider-specific blockers. In particular, the Hugging Face observation
  applies to one downstream provider, and Hyperbolic's cache fields vary by
  model. The broad model-list refresh provides no new wire evidence for these
  unverified claims.
