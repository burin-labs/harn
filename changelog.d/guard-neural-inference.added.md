- **On-device neural injection classifier (Layer 2, inference).** `harn-guard` gained the ONNX
  inference backend behind the off-by-default `guard-neural` cargo feature: it loads an installed model
  package (`~/.harn/guard/<name>/` — ONNX graph + `tokenizer.json` + `config.json`) and scores untrusted
  content with a transformer sequence-classifier, superseding the built-in heuristic for better recall.
  The runtime resolves the model named by the new `[security] guard_model` config key lazily — on the
  first scored span, never at startup — via a new `harn-vm` loader seam
  (`set_injection_classifier_loader` / `ensure_neural_classifier`) that keeps `harn-vm` free of any
  inference dependency. A transient inference error degrades to the heuristic rather than dropping
  detection. The default binary links no model runtime; CI never downloads weights.
