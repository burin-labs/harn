`AcpServerConfig` and `DispatchCoreConfig` gain an optional typed
`host_inference_boundary` field. Library embedders using struct literals must
set it; the existing constructors initialize it to `None`. Set a validated
`InferenceBoundary` when the host requires a ceiling independent of client
session environment choices.
