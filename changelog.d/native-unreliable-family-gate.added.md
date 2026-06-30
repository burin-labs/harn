Added a capability-matrix consistency gate that forbids any route from pinning
the provider-native tool channel for a model family whose native channel is
unreliable as a weight-intrinsic property (starting with GLM-5.x, which leaks
`<tool_call>` markup into assistant content instead of returning native tool
calls). Fixed the NVIDIA GLM-5 route — the lone outlier that pinned `native`
while the rest of the family uses the clean text channel — so a value model can
no longer silently thrash on a single mis-pinned provider.
