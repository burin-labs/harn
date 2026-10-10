- **The inference admission snapshot reports the resolved data posture.**
  `harn provider admission` and `preview_inference_admission` now return
  `data_posture` (the posture inference will send on the route),
  `data_controls_outcome`, and `data_controls_note` (the route's own handling
  note when its model row declares one). A route the `strictest_available`
  posture refuses now previews as `denied` with governing rule
  `data_controls.training_refused` and the typed
  `data_controls_refusal: training_without_control`, instead of `admitted`.

  Migration: code that builds an `InferenceAdmissionSnapshot` literal adds the
  four fields, for example `data_posture: DataPosture::Default,
  data_controls_outcome: None, data_controls_note: None,
  data_controls_refusal: None`.
  Hosts decoding the JSON need no change.
