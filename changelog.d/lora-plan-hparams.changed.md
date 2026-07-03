`harn models lora plan` now records LoRA rank, alpha, and dropout in the training
contract and propagates the planned rank into local launch hints when the runtime
supports max-rank configuration.
