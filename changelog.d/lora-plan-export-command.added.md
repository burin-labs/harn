- **`harn models lora plan` now prints the matching export command.** The plan
  includes a manifest-backed `harn models lora export` invocation so LoRA
  dataset export, trainer inputs, evals, and serving all share the same resolved
  model/tool-call contract.
