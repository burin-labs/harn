A2A streaming tasks keep progress-event sinks registered until terminal task publication,
preventing final progress updates from being dropped at dispatch shutdown.
