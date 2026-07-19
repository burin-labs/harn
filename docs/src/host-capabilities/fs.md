# Filesystem Host Capabilities

Filesystem access is exposed through the capability-aware `harness.fs`
sub-handle. The legacy free filesystem builtins remain available for existing
scripts, but new code should use `harness.fs.*` so `harn graph`, lint repairs,
and host policy checks can attribute filesystem effects to the typed harness
surface.

| Method | Backing builtin | Capability |
|---|---|---|
| `harness.fs.read_text(path)` | `read_file(path)` | `workspace.read_text` |
| `harness.fs.read_text_result(path)` | `read_file_result(path)` | `workspace.read_text` |
| `harness.fs.read_bytes(path)` | `read_file_bytes(path)` | `workspace.read_text` |
| `harness.fs.write_text(path, content)` | `write_file(path, content)` | `workspace.write_text` |
| `harness.fs.write_bytes(path, content)` | `write_file_bytes(path, content)` | `workspace.write_text` |
| `harness.fs.replace_text(path, content, options?)` | `replace_file(path, content, options?)` | `workspace.write_text` |
| `harness.fs.replace_text_result(path, content, options?)` | `replace_file_result(path, content, options?)` | `workspace.write_text` |
| `harness.fs.replace_bytes(path, content, options?)` | `replace_file_bytes(path, content, options?)` | `workspace.write_text` |
| `harness.fs.replace_bytes_result(path, content, options?)` | `replace_file_bytes_result(path, content, options?)` | `workspace.write_text` |
| `harness.fs.exists(path)` | `file_exists(path)` | `workspace.exists` |
| `harness.fs.status(path, access?)` | `path_status(path, access?)` | `workspace.exists` |
| `harness.fs.delete(path)` | `delete_file(path)` | `workspace.delete` |
| `harness.fs.append(path, content)` | `append_file(path, content)` | `workspace.write_text` |
| `harness.fs.append_locked(path, content, options?)` | `append_file_locked(path, content, options?)` | `workspace.write_text` |
| `harness.fs.list_dir(path?)` | `list_dir(path?)` | `workspace.list` |
| `harness.fs.mkdir(path)` | `mkdir(path)` | `workspace.write_text` |
| `harness.fs.copy(src, dst)` | `copy_file(src, dst)` | `workspace.write_text` |
| `harness.fs.temp_dir()` | `temp_dir()` | none |
| `harness.fs.workspace_temp_dir()` | `workspace_temp_dir()` | `workspace.write_text` |
| `harness.fs.mkdtemp_in_workspace(prefix?)` | `mkdtemp_in_workspace(prefix?)` | `workspace.write_text` |
| `harness.fs.mkdtemp(prefix?)` | `mkdtemp(prefix?)` | `workspace.write_text` |
| `harness.fs.stat(path)` | `stat(path)` | `workspace.exists` |
| `harness.fs.rename(src, dst)` | `move_file(src, dst)` | `workspace.write_text` |
| `harness.fs.read_lines(path)` | `read_lines(path)` | `workspace.read_text` |
| `harness.fs.walk(path, options?)` | `walk_dir(path, options?)` | `workspace.list` |
| `harness.fs.glob(pattern, base_or_options?, options?)` | `glob(pattern, base_or_options?, options?)` | `workspace.list` |
| `harness.fs.find_text(root, pattern, options?)` | `find_text(root, pattern, options?)` | `workspace.list` + `workspace.read_text` |
| `harness.fs.find_evidence(roots, patterns, options?)` | `find_evidence(roots, patterns, options?)` | `workspace.list` + `workspace.read_text` for every root |

`harness.fs.read_text_result(path)` returns a closed structured I/O failure
with stable `kind` values such as `not_found`, `permission_denied`,
`invalid_data`, and `sandbox_denied`. Branch on `kind`; keep `message` for
diagnostics rather than parsing its prose.

The replacement methods update a complete file only when the optional
`expected_sha256` lease still matches. They return `created`, `replaced`,
`no_op`, or `stale`; stale is a successful receipt and never writes. Digests
use lowercase `sha256:<64 hex digits>`. `create`, `overwrite`, and
`create_parents` default to true. Symlink destinations are rejected.

The default `namespace` durability means readers cannot observe a partial
payload. `{durability: "flush"}` also requests a payload and namespace flush;
`file_synced` and `namespace_synced` report what the operating system
completed. These fields do not claim stronger guarantees than the filesystem
or storage hardware provides. Import the typed `replace_text[_result]` and
`replace_bytes[_result]` wrappers from `std/fs` for application code.

`harness.fs.workspace_temp_dir()` returns a workspace-local scratch directory,
creating it lazily. Sandboxed runs place the directory inside the active
workspace root; unsandboxed runs use `.harn-tmp` relative to the script source
root.

`harness.fs.mkdtemp_in_workspace(prefix?)` creates a uniquely named directory
under `harness.fs.workspace_temp_dir()`. Prefer this for intermediate files
that later filesystem or process calls must read under the same sandbox policy.

`harness.fs.mkdtemp(prefix?)` creates a uniquely named directory under the host
temporary directory and returns its absolute path. Use it only for host-temp
work that does not need to be visible through workspace sandbox rules. The
directory is not automatically removed; callers own cleanup with
`harness.fs.delete(path)`.

`harness.fs.glob(pattern, base_or_options?, options?)` returns the same sorted
matches as `glob(...)`. Patterns are matched against forward-slash paths
relative to the base directory, and `long_running` / `background` options return
a long-running operation handle.

`harness.fs.find_text(root, pattern, options?)` walks with gitignore-aware
defaults and searches matching files in the VM. It returns a list of
`{path, line, col, column, text}` hits by default. Set `mode: "exists"` for a
boolean short-circuit or `mode: "count"` for an integer count. The search is
fixed-string by default for lint/source-guard workloads; pass
`{fixed_strings: false}` to treat `pattern` as a regular expression.
`preset: "source"` adds common source-tree excludes (`node_modules`, `target`,
`dist`, `.git`, `.harn-runs`, `vendor`) and a 1 MiB file-size ceiling;
`preset: "all"` disables hidden-file and ignore filtering. Use `include`,
`exclude`, `ignore`, or their `*_globs` forms with glob strings or lists for
explicit overrides. Count mode is capped by `max_matches` (default 1000).
Summary modes can set `parallel: true` and optional `threads` for a parallel
walker.

`harness.fs.find_evidence(roots, patterns, options?)` and its ambient builtin
accept labeled `{id, path}` roots
and labeled `{id, text}` literals. It walks each root once, matches all literals
with one matcher, and returns deterministic path-relative hits plus settled
per-root failures and match-budget receipts. Root paths are not copied into the
receipt. Its `case_insensitive` option folds ASCII letters. Set `long_running`
or `background` for the standard cancellable operation handle. Import
`search_evidence` or `search_evidence_background` from `std/fs` for typed Harn
options and results.

For direct CLI runs, `harn run --write-root <path>` adds an external writable
root to the same sandbox policy used for the primary workspace. Prefer it over
`--no-sandbox` when a script needs to update a declared output folder outside
the project tree. `--read-only-root <path>` remains the additive read-only
variant for shared assets or sibling repositories.
