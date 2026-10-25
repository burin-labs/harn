Consumer observation retries transient GitHub reads against the original run and budget, preserving
checkpoints and server backoff across credential renewal. A failed observer no longer cancels a live
measurement unless cancellation was explicit or fresh authenticated jobs prove a timeout.
