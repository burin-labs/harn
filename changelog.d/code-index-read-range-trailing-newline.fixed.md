- Stop `code_index` `read_range` from over-reporting line totals by one and
  returning a phantom empty last line for newline-terminated files, matching
  the canonical `count_lines` semantics.
