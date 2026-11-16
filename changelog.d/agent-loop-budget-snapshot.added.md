- **Agent tools and terminal callbacks can read the loop's current budget (#7619).**
  `harness.agent.session_loop_budget(session_id)` returns the existing loop-owned
  iteration and grant projection, with an unavailable measurement distinct from
  zero. Reads don't change budget or terminal retry policy.
