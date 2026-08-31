Model selection now resolves the requested name and provider as one typed
decision. Built-in provider selectors cannot cross catalog namespaces, custom
adapters can deliberately proxy upstream model identities, alias near-misses
fail with catalog-versioned suggestions, provider-call receipts record the
complete requested-to-resolved route, and
`std/llm/catalog.execution_contract` returns the stricter
`harn.llm.execution-contract/v2` shape.
