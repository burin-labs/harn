- An accepted mid-run steer now updates what the run is obligated to deliver.
  The completion judge previously read only the task the session opened with, so
  a user who redirected a run mid-turn — "stop doing that, report what you found
  instead" — would watch the judge veto the very stop they had authorized and
  order the withdrawn work redone. The judge's prompt now carries the accepted
  steering with explicit supersession authority over the completion goal, the
  rubric, and any requirement row frozen at loop start.
- The amended completion target is derived once, at the judge payload seam, and
  carried on the terminal evidence snapshot, so a `verify_completion` closure and
  the completion gate read the same obligations the judge does rather than
  re-deriving a goal from raw transcript history. A steer is identified by the
  typed delivery mode the host bridge now stamps on each delivered user message,
  not by transcript position; an `audit_only` note the model never saw cannot
  change the completion target.
- A run with no accepted steer renders no steering block, keeps a byte-identical
  judge prefix, and keeps an unchanged terminal evidence identity. The judge's
  authority over a lazy or unverified stop is unchanged.
