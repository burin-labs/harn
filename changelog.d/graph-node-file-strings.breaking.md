- **Code-index graph nodes share their file's strings.** In
  `harn_hostlib::code_index::Node`, `path` is now `Arc<str>`, shared by every
  node of a file, and `language` is a `&'static str` from the language
  registry. Snapshots store each file's path and language once instead of on
  every node, which makes them smaller and faster to restore, so the first
  start after upgrading rebuilds the index once.

  Migration: compare and borrow through the shared string, and convert where
  an owned `String` is needed:

  ```rust
  if &*node.path == "src/lib.rs" && node.language == "rust" {
      let owned: String = node.path.to_string();
  }
  ```
