- **`harn guard` — downloadable on-device injection-detection models (Layer 2, management).** A new
  `harn-guard` crate and `harn guard {list,install,status,remove}` CLI manage prompt-injection
  classifier models under `~/.harn/guard/`. The catalog points at already-hosted upstream models (Harn
  hosts nothing, bundles no weights); `install` fetches on the user's machine, verifies SHA-256 against
  the catalog's pinned digests, and requires explicit `--accept-license`. The default model
  (`deberta-v3-prompt-injection-v2`) is Apache-2.0 and ungated; gated models (e.g. Meta Llama Prompt
  Guard 2) are opt-in and require the user's own `HF_TOKEN`. The neural inference runtime is behind the
  off-by-default `guard-neural` cargo feature, so the default binary stays lean and falls back to the
  built-in heuristic classifier.
