- **Text-tool agent history now follows the route's effective tool channel
  (#6353).** When a provider returns structured tool calls on a text-only route,
  Harn reserializes the assistant call and result into the selected text grammar
  instead of replaying an invalid native tool role on the next turn.
