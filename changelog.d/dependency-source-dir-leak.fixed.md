- Dependency package loads no longer leak the thread-local source dir, so
  top-level `@asset`/relative prompt resolution anchors on the project root even
  when `[dependencies]` are present.
