Tool-call parse errors for an unquoted multi-line value (e.g. a raw code body
pasted after `content:`/`new_text:`) now name the heredoc recovery
(`key: <<BODY … BODY`) instead of dead-ending on "unexpected character starting
a value", so a weak value model can self-heal instead of looping on the same
malformed edit.
