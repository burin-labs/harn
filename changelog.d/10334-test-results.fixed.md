`harn test` now fails a test or file fixture that returns `Err`, including errors
propagated with `?`, instead of reporting a pass before assertions run.
`harn bench` also refuses to record an iteration that returns `Err` as a success.
