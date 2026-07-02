Tool-ceiling denials no longer use permission framing: an unknown/excluded
tool name now gets action-oriented "not one of the available tools" feedback
(listing the callable tools), and a call named `tool_call` whose arguments
smuggle one valid text-format call gets parse-repair feedback that names the
embedded call and shows the direct invocation. Previously both fell into the
generic "tell the user what you need permission for" denial body, which sent
headless models into permission-request spirals with no user to ask.
Genuinely permission-gated denials (capability/side-effect ceilings, approval
and host rejections) keep the existing wording.
