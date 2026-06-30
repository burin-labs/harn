- **Orchestrator pump startup readiness.** The in-process orchestrator harness now
  waits for pending, inbox, cron, and waitpoint pumps to subscribe before it
  reports startup readiness, preventing immediately accepted trigger requests
  from racing ahead of the pump cursor and being skipped under CI scheduling.
