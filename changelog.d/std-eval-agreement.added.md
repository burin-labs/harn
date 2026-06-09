Added `std/eval/agreement`: deterministic, I/O-free cross-checked-success math for eval ledgers — the reusable
counterpart to `std/eval/stats`. Exposes `agreement_decision` (the ">=2 independent judges must agree, with at least one
independent re-execution among them" rule) and `cohen_kappa` (inter-judge agreement statistic), so eval clients can drop
their own hand-rolled agreement math.
