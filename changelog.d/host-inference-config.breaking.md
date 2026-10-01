`AcpServerConfig` and `DispatchCoreConfig` gain an optional typed
`host_inference_boundary` field. Library embedders using struct literals must
set it; the existing constructors initialize it to `None`. Set a validated
`InferenceBoundary` when the host requires a ceiling independent of client
session environment choices.

Migration: Set `host_inference_boundary: None` in public struct literals to
preserve prior behavior, or supply a validated typed floor for host policy.
