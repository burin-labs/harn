- **LoRA planning now emits post-training receipt and probe commands.** `harn
  models lora plan` includes the `harn models lora manifest` handoff step and a
  served-route `harn provider tool-probe` command so external trainers can return
  auditable adapter metadata to Harn before inspection, launch, and promotion
  evals.
