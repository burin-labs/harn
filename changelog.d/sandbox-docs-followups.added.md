- **Sandbox write roots and portable document PDF rendering.** `harn run` and
  `harn time run` now accept repeatable `--write-root` / `--writable-root`
  flags for sandboxed writes to declared external output folders, and
  `std/document` adds dependency-free text, HTML, and Markdown-to-PDF helpers
  backed by Harn's built-in `document_render_pdf` primitive.
