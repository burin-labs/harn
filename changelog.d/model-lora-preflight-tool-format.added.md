- **LoRA preflight now validates the target tool-call format.** `harn models
  lora preflight` accepts `--tool-format`, checks that the corpus source tool
  calls can export into the selected Harn/native route, and `harn models lora
  plan` now prints a matching preflight command before dataset export.
