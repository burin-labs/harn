Agent hosts can use `harness.agent.with_run(session_id, options, body)` to
record preparatory model calls and the agent loop in one durable run, including
reasoning receipts and physical provider usage. Preparation failures and
zero-call runs retain explicit terminal evidence.
