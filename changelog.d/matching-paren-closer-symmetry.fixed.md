Fix `matching_paren_len` (stdlib public-function signature parser) so a top-level `]` or `}`
terminates the scan when bracket depth returns to zero, matching its symmetric opener arm and the
sibling `split_top_level_params`. Previously only `)` triggered the return, so a mismatched closing
bracket made the scan run to the end and yield `None`.
