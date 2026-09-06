A host event payload key that no field of the resulting event reads is now
reported instead of discarded in silence. `AgentEvent` is an internally-tagged
enum with no `deny_unknown_fields`, so an emitter could write a key, the emit
could succeed, the run could succeed, and the field would simply not be there
for whoever read the timeline afterwards to work out what happened. The loss is
now a typed `dropped` record on the boundary funnel, carrying the event type and
the key name, reported once per session rather than once per turn.

It is a report and not a refusal on purpose. Live emitters are known to pass
keys nothing consumes, some deliberately, so refusing the event would turn an
invisible loss into a dead run. The registry that already owns this boundary now
also records how much of a payload each arm reads, which is what separates a key
nobody reads from one read under another name.
