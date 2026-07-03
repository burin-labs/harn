Added `std/verification::verification_gate_input`, a structured reducer that
combines stale-diagnostic classification with diagnostic-delta progress credit
for loop gate policy. Harn's agent stall detector now uses that reducer so
stale or unbound diagnostics stay advisory instead of feeding no-progress
streaks.
