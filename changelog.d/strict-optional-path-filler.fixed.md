- An optional path argument sent as `""` is now treated as absent at dispatch instead of being
  refused as a malformed path. OpenAI strict mode makes models send every optional property, and
  they fill unused strings with `""`, so a tool such as a `look` with optional `file` and `folder`
  failed its first call. A required path that is empty is still refused.
