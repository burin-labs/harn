- The runtime native→text tool-format degrade now also fires on a
  billed-noncommittal vanishing tool call (the upstream finished cleanly, billed
  output, and committed no tool call — the action stranded in a private reasoning
  channel) and on a native function-call protocol refusal (e.g. SambaNova's HTTP
  400 "Model started a function call but did not complete it"), not only on the
  5xx/EOF server-side parser choke it already handled. These signatures meant a
  native-channel route that vanished its tool call previously retried the same
  broken native channel until the budget drained, then surfaced; now it degrades
  once to the text channel and recovers. The degrade stays a one-way last resort:
  it remains gated to native channels, fires at most once per call, and never
  triggers for a `length`/`max_tokens` truncation (continue-on-truncation stays
  above channel-switch in the remedy order).
