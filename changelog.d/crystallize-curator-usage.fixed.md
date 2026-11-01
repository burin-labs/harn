`crystallization_curate` now returns `usage`, the curator run's physical provider ledger (tokens and cost). Hosts that
read usage from the outcome previously found none and reported an empty record. It is absent when the run measured
nothing.
