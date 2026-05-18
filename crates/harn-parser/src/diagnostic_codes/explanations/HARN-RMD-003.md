# HARN-RMD-003

**Variant:** `Code::ReminderUnsupportedUserBlockRoleHint`

The pipeline hardcodes `role_hint: "user_block"` while also selecting an LLM
provider/model route that cannot render reminders as Anthropic-style user
content blocks or OpenAI developer-role messages.

Use `role_hint: "system"` or `role_hint: "developer"` for provider-neutral
reminders, or branch on provider capability flags before selecting a
provider-specific reminder shape.
