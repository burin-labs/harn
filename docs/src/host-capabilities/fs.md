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
| `harness.fs.exists(path)` | `file_exists(path)` | `workspace.exists` |
| `harness.fs.delete(path)` | `delete_file(path)` | `workspace.delete` |
| `harness.fs.append(path, content)` | `append_file(path, content)` | `workspace.write_text` |
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
