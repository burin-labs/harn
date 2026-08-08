---
name: a2a-reviewer-fanout
short: Customize an A2A push fanout trigger pipeline.
description: A2A trigger example for routing remote reviewer updates.
when-to-use: Use when forwarding work between Harn orchestrators.
---
# A2A reviewer fanout

Customize the handler target and payload shape in `lib.harn`. Keep transport
auth in the A2A layer rather than adding provider secrets to this trigger.
