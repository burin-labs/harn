Added `estimate_cost_usd` and `realized_trial_cost_usd` to `std/eval/stats`: cache-aware token→USD cost estimation
(cache-read/write tokens are billed at their own rates and not re-charged at the full input rate) plus cached-replay
realized-cost accounting. Lets eval harnesses drop their own hand-rolled LLM cost math.
