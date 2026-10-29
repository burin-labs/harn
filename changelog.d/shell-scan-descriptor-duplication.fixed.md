- `command_risk_scan` now resolves shell commands that duplicate or close a file
  descriptor (`2>&1`, `>&2`, `2>&-`). They used to report
  `dynamic_or_invalid_execution_syntax`, so fail-closed shell guards refused
  ordinary commands such as `git -C dir commit -m x > log 2>&1`. A descriptor
  named at run time (`>&$fd`) stays unresolved, and `>&file` is reported as the
  `&>` file write it performs.
- The agent shell guard no longer treats every backslash as a possible
  `PATH` assignment, which refused `git -C dir grep "a\|b"` with a misleading
  `git stash` message. Only a line continuation can splice a variable name.
