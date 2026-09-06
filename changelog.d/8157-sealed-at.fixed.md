A run record's terminal now says when it was sealed. The record already dated the
run's end from that same stamp, but only as `finished_at`, whose provenance a
reader had to look up in `run_clock` before knowing it came from the terminal at
all. `metadata.terminal.sealed_at` carries it directly, so a reader asking when a
run stopped no longer has to join two fields to find out whether the stop was the
end of the run.
