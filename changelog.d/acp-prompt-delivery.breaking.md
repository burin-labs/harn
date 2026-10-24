- **ACP mid-turn user input now follows the prompt delivery RFD.** A
  `session/prompt` with `delivery: "steer"` joins the running turn at its next
  safe point; `delivery: "queue"` starts a new turn after it. The response is
  `{messageId}` once the message is inserted. A delivery prompt to an idle
  session starts a turn, reported by `state_update` session updates.
  `$/cancel_request` withdraws a prompt that has not been inserted, and
  `session/close` or a closed connection fail every pending one with
  `-32800`. `session/cancel` stops only the running turn; pending steers then
  open the next turn. Agents advertise
  `promptCapabilities.delivery = {"steer": {}, "queue": {}}`.

  Removed: `session/inject`, `session/revoke_inject`, `session/replace_inject`,
  the `session.inject` capability, and `EmbeddedAgent::inject_user_input`.
  `session/pending_injections` is renamed `session/pending_prompts` and returns
  `{prompts, reminders}`. `interrupt_immediate` is no longer reachable over ACP.

  Migration: send `session/prompt` with `delivery` and a `prompt` content-block
  array in place of `session/inject` with `mode` and `content`. Move `goal` to
  `_meta.harn.goal`. Withdraw with `$/cancel_request` instead of
  `session/revoke_inject`; there is no replacement for `session/replace_inject`.
  To deliver now, send a steer and then `session/cancel`. A never-delivered
  audit note belongs on `session/remind` with `audit_only`.
  Use `EmbeddedAgent::deliver_user_input`.
