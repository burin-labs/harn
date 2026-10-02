- `harn provider option-probe` no longer reads an OpenRouter option as supported when OpenRouter silently
  dropped it: probe requests now set `provider.require_parameters`, and OpenRouter's "No endpoints found
  that can handle the requested parameters" counts as a rejection. A rejection now needs a passing
  control request without the option before it counts as evidence, a transport timeout is retried
  once, and an account data-policy refusal is reported as a skip with its reason instead of an error.
