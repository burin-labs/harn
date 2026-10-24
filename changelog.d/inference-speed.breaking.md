- **`AgentEvent` gains an `InferenceSpeed` variant and `LlmTelemetry` gains
  an `inference_speed` field.** Rust embedders that match `AgentEvent`
  exhaustively, or build `LlmTelemetry` with a struct literal, no longer
  compile until they handle the new variant and field.

  Migration: add an arm for the new variant (or a wildcard) and fill the new
  field (or spread a default).

  ```rust
  // Before
  match event { AgentEvent::IterationEnd { .. } => {} /* ... */ }
  LlmTelemetry { client_first_frame_ms, ..LlmTelemetry::default() };
  // After
  match event {
      AgentEvent::InferenceSpeed { speed, .. } => show(speed),
      AgentEvent::IterationEnd { .. } => {}
      /* ... */
  }
  LlmTelemetry { client_first_frame_ms, inference_speed: None, ..LlmTelemetry::default() };
  ```
