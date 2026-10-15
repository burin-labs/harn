- The project scanner extracts files in parallel and skips the tree-sitter
  pass for files over 1 MiB (generated sources take the line-oriented
  fallback). Scanning this repository went from about 8.6 s back to about
  1.1 s, level with the scanner before it switched to tree-sitter symbols.
- C++ member function declarations such as `int x() const;`, including ones
  returning pointers or references, are reported as methods instead of fields
  named `int x`. Function-pointer members stay fields.
