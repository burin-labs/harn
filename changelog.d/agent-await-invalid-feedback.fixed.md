- **Agent loop lifecycle calls recover from invalid self-suspension arguments.**
  Malformed `agent_await_resumption` calls now inject corrective feedback and
  let the model continue instead of aborting the whole turn with
  `HARN-SUS-002`.
