- Reject a sliding stream window whose `step` exceeds its `size`. Such a
  config silently collapsed to tumbling (the intended gap events were
  never skipped); it is now rejected at validation time with a clear
  error. Overlap (`step < size`) and the tumbling-equivalent
  (`step == size`) are unchanged.
