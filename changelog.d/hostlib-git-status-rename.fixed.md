- Decode staged rename/copy records in the `tools.git` `status` operation
  instead of mis-parsing the NUL-separated original-path field as a garbage
  entry, and expose the source path as `orig_path`.
