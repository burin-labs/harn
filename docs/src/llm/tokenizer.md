# Exact token references

Use `std/llm/tokenizer` when an API option needs token IDs. A token ID is only
meaningful inside one vocabulary. Harn therefore represents it as `TokenRef`,
not as a bare integer.

```harn
import { TokenBias, detokenize, tokenize } from "std/llm/tokenizer"
import { LlmCallOptions } from "std/llm/options"

fn main(harness: Harness) {
  const tokens = tokenize(" spoilers", "gpt-4o")
  const token = tokens[0]
  if token == nil {
    throw "tokenization returned no tokens"
  }
  const biases: list<TokenBias> = [{token: token, bias: -8.0}]
  const options: LlmCallOptions = {
    provider: "openai",
    model: "gpt-4o",
    logit_bias: biases,
  }
  const result = harness.llm.call(
    "Summarize without spoilers.",
    nil,
    options,
  )
  assert(len(result.text) > 0, "model returned text")
  assert_eq(detokenize(tokens), " spoilers")
}
```

## Types

`TokenRef` has this closed shape:

```harn
pub type TokenRef = {
  _type: "llm_token",
  id: int,
  tokenizer: string,
  bytes: list<int>,
  text: string?,
}
```

`tokenizer` is an exact vocabulary identity such as
`tiktoken:o200k_base`. `text` is `nil` when one token contains only part of a
UTF-8 character. `bytes` preserves that fragment without lossy conversion.

`TokenBias` is `{token: TokenRef, bias: float}`. Bias must be finite and within
`-100..=100`. A list can't contain the same token ID twice.

## Functions

| Function | Return | Behavior |
|---|---|---|
| `tokenize(text, model)` | `list<TokenRef>` | Uses the model's exact local vocabulary. It errors when Harn only has an approximate counter. |
| `detokenize(tokens)` | `string` | Decodes one vocabulary-scoped sequence. It rejects mixed vocabularies and invalid UTF-8. |
| `unsafe_token_ref(id, tokenizer)` | `TokenRef` | Builds a reference for an integration that already has an exact ID. The caller must supply the vocabulary identity. |

Harn's Claude and Gemini token counters are estimates. They can't create
`TokenRef` values. This prevents an estimate based on `cl100k_base` from being
mistaken for a provider token ID.

## Route validation

`logit_bias` is checked twice. Its token references must agree with the model
selected during option parsing, and admission runs again after each routing or
fallback decision. Harn stops before network I/O if the route has no authored
token-bias lowering or uses another vocabulary.

Biasing every token returned for a multi-token string affects each token
independently. It does not ban or require that token sequence as a phrase.

See [LLM call generation options](./llm_call.md#generation) for limits and the
[provider capability matrix](../provider-matrix.md) for current route support.
