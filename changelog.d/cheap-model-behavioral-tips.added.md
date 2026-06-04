- **Behavioral tips to cut cheap/local-model toolless churn and format
  leakage.** The agent loop now recovers from the most common
  weak-model failure habits without changing the wire protocol. New TEXT-mode
  corrective nudges fire on the turns the native-gated completion confirmation
  never reached: a `fenced_call_attempt` nudge when a call is wrapped in a
  ```` ```tool_code ````/`call`/`edit`/`python` Markdown fence the parser
  ignores, and a `named_tool_not_called` nudge when the model narrates a bound
  tool ("I'll use `edit`…") but emits no call. A decaying "turns since
  meaningful progress" counter drives an escalating `no_progress_streak` nudge
  for pure-prose churn — it does not fully reset on a single dispatch, and the
  content-specific nudges take precedence so a turn is never double-nudged. The
  text response-protocol prompt also hoists an anti-fence rule, an
  object-literal-vs-Python-kwarg rule, a heredoc-close reminder, and a
  "no code in `<user_response>`" rule. All detectors are conditioned on observed
  output shape, not on any model name.
