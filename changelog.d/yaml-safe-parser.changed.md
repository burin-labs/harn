- YAML now goes through one safe-Rust parser, serde-saphyr, replacing serde_yaml_ng and the unmaintained
  unsafe-libyaml (RUSTSEC-2024-0449). `yaml_parse`, `read_yaml`, SKILL.md frontmatter, and project
  scanning share it. Only `true` and `false` are booleans, as before. Behavior changes: merge keys
  (`<<: *anchor`) now expand; unquoted numbers with leading zeros (`02134`) and digit separators
  (`1_000`) parse as numbers, so quote them to keep the text; `.inf` and `.nan` decode as the strings
  `".inf"` and `".nan"` instead of `nil`; custom tags such as `!env x` keep the value and drop the tag;
  parse error messages are worded differently. `yaml_stringify` quotes strings that would otherwise
  read back as another type (for example `"yes"`) and uses double quotes.
