- **The formatter no longer drops or relocates comments on multi-line binary
  and pipe expressions.** A trailing comment on the first line of an
  expression that breaks across lines (e.g. `let r = aaa // note` followed by
  `+ bbb`) was silently dropped at the top level, or moved out of its
  enclosing block to the end of the file inside a function. Each broken
  operand's trailing comment is now preserved in place.
