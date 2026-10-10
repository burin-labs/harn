`harn provider admission` and `preview_inference_admission` now report the data posture
inference will send on the route (`data_posture`), what it does there
(`data_controls_outcome`), and the route's own handling note (`data_controls_note`).
A route the `strictest_available` posture refuses now previews as `denied` with
governing rule `data_controls.training_refused` instead of `admitted`.
