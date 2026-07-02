`normalize_tool_call_shape` now folds a top-level `create` call into `edit({action: "create"})` like the
other edit-action verbs, and a new negative test pins that shell listing `run` commands (`ls -R`, `tree`)
pass through untouched — structured-listing ergonomics belong to the host.
