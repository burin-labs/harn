Scoped deadlines now use the runtime clock for asynchronous waits, so paused or
advanced test time cannot make them expire early or miss a timeout. Blocking
host calls receive the remaining deadline budget on their real clock.
