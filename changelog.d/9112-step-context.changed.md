- The interpreter shares typed step-context data with the native runtime and
  restores suspended caller context through a runtime-owned callback. Function
  entry, frame pruning, and source-directory changes use the required runtime
  boundary without boxing the context guard on each callback.
