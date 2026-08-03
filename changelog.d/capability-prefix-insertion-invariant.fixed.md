- **A capability repair no longer shifts a call's ordinary arguments one slot.**
  A diagnostic names one argument slot, not the callee's declared capability
  prefix, so when a callee took several capabilities and the call omitted all of
  them, the reported slot was the second or later one. Inserting there left the
  preceding ordinary argument sitting in a capability position and moved every
  later argument one slot along, turning an incomplete call into a wrong one
  whose damage resurfaced as an unrelated type error a slot away. Capability
  parameters are always a contiguous leading prefix, so a lone insertion is now
  taken only when every preceding argument already carries a capability. Calls
  that fail that test belong to the whole-program pass, which reads the callee's
  declared prefix from the module graph and inserts it whole; when that pass
  cannot resolve the call, the site is left intact for a human instead of being
  silently shifted.
