- **`std/agent`: no-net-progress stall detection now has a hard failing-verify
  floor.** When `stall_diagnostics.no_net_progress_extend_guard` is enabled,
  Harn counts failing verification turns across changing diagnostic signatures
  and successful edit calls, resets only on a clean verification pass, and emits
  a terminal `no_net_progress_hard_cap` stuck warning once the hard cap is
  crossed. This prevents slow-draining red-build loops from evading the existing
  same-diagnostic no-net-progress guard.
