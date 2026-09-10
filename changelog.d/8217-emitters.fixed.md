Repair the agent-loop emitters that wrote host event payload keys no reader
could see. The budget stop now records the iteration it happened on; the
auto-continue, overflow-recovery and blank-tool-name receipts name the stop
reason, provider error and dispatch count that caused them; every synthesized
feedback receipt keeps the turn it fired on; and the emitters that repeated a
sibling event's facts stop claiming to record them.
