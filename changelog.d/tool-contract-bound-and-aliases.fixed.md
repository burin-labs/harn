- Cheap-model tool-call dialect: three fixes that stop advertised-only-tool
  lanes (e.g. the Burin eval lane: `look`/`search`/`edit`/`run`/
  `read_command_output`) from denying calls models emit in another harness's
  vocabulary. (1) The tool-calling contract (both the text and fenced-JSON
  prompts) now states under `## Available tools` that these are the ONLY callable
  tools and that any unlisted name is rejected — pick the closest listed tool;
  the JSON contract's worked `## Example` no longer primes the unlisted
  `write_file` name (it now uses an illustrative `<tool>` placeholder). (2) The
  tool-name normalizer resolves SEMANTIC aliases to canonical Harn tools so the
  gate, dispatch, and telemetry all see a real name: `repo_browser.*` /
  `repository_browser.*` / `workspace_browser.*` / `file_browser.*` file/list
  verbs → `look`, their search/find/grep verbs → `search`; `container.exec` /
  `container_exec` / `exec` / `sh` / `shell` / `bash` → `run` (remapping a
  `script` / `cmd` arg onto `command`); and edit-action verbs called as
  top-level tools (`replace_range`, `replace_body`, `insert_after`,
  `insert_function`, `delete_range`, `exact_patch`, `add_import`) →
  `edit({ action: <verb>, … })`. Raw-write/whole-file tools (`write_file` /
  `delete_file` / `patch_file`) are deliberately NOT aliased to `edit` — they are
  semantically lossy — and the symbol-level edit tools (`replace_symbol` /
  `remove_symbol`) are NOT folded into `edit` either, since `replace_symbol` is a
  hard-kept standalone tool in the default surface; all fall through to the
  denial feedback instead. (3) The permission-denial feedback now NAMES the active
  policy's allowed tools (`… Available tools: look, search, edit, run,
  read_command_output.`) so the model can self-correct in one turn. Sibling to
  the `tool.`/`functions.` namespace-prefix strip.
