- Added a top-level `harn doc [path]` command that renders Markdown API reference docs for a Harn file or
  project's `pub` symbols (functions, consts, types, enums, structs) drawn from their HarnDoc comments —
  signature, description, parameters, `@effects`, and `@errors`. Prints to stdout by default, or writes a file
  with `--output <file>`. Unlike `harn package docs`, which only documents modules declared in a `harn.toml`
  `[exports]` table, `harn doc` walks the target path directly, so a plain project produces real reference docs
  for every `pub` symbol it defines. Reuses the package pipeline's HarnDoc extractor and per-symbol renderer;
  `pub const` declarations are now recognized by both. HTML sites and doc-example testing remain future extensions.
