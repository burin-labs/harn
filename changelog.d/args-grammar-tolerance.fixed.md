- **Tool-call argument grammar now accepts the code-bearing value shapes weak
  value models naturally emit**, instead of dropping the turn to
  parse-guidance. Three shapes from the transcript corpus now parse and
  canonicalize back to `name({ ... })` on replay: (1) `+`-concatenated
  string/template fragments — including the multi-line backtick template
  literals and `` `…` + "`json:\"x\"`" + `…` `` struct-tag concatenation Go
  forces — collapse into one string value; (2) a heredoc whose closing tag is
  indented, misspelled, or omitted but whose call is structurally closed (a
  trailing `})`/`)` call-tail) is implicitly terminated at that tail; and (3)
  `=` is accepted as a synonym for `:` as the object key/value separator
  (`{ new_body= <<EOF … }`). A flat JSON-RPC/MCP envelope
  (`[{"name":"read","arguments":{…}}]` or a single object with `parameters`)
  also maps to the matching call. The recover/reject boundary stays sharp: a
  `+` with a non-string right operand, a heredoc body truncated mid-token with
  no structural tail, an ambiguous bare-`}` code close, and prose JSON that
  merely has a `name` key all still error loudly.
