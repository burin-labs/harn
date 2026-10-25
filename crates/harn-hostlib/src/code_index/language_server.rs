//! `code_index.language_server` — whether a language server would answer a
//! rename for a file, decided by the lookup the rename itself uses and without
//! starting a process.
//!
//! A caller that wants to say "one `rename_symbol` call renames this" needs
//! to know whether the rename can reach a server: without one, a name with
//! competing declarations is refused. Answering from the same lookup keeps the
//! claim and the rename from disagreeing.

use std::path::{Path, PathBuf};

use harn_vm::VmValue;

use super::builtins::SharedIndex;
use super::lsp;
use crate::error::HostlibError;
use crate::tools::args::{
    build_dict, dict_arg, optional_string, require_string, resolve_host_path, str_value,
    to_agent_path,
};

pub(super) const BUILTIN: &str = "hostlib_code_index_language_server";

pub(super) fn run(index: &SharedIndex, args: &[VmValue]) -> Result<VmValue, HostlibError> {
    let raw = dict_arg(BUILTIN, args)?;
    let dict = raw.as_ref();
    let path = require_string(BUILTIN, dict, "path")?;
    let root = match optional_string(BUILTIN, dict, "root")? {
        Some(root) => resolve_host_path(root),
        None => index
            .lock()
            .expect("code_index mutex poisoned")
            .as_ref()
            .map(|state| state.root.clone())
            .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))),
    };
    let root = std::fs::canonicalize(&root).unwrap_or(root);
    let relative = relative_path(&root, Path::new(&path));
    let lookup = lsp::lookup_server(&root, &relative);
    let nil_or = |value: Option<VmValue>| value.unwrap_or(VmValue::Nil);
    Ok(build_dict([
        ("path", str_value(&path)),
        ("resolvable", VmValue::Bool(lookup.server.is_some())),
        (
            "language",
            nil_or(lookup.language.map(|language| str_value(language.name()))),
        ),
        (
            "server",
            nil_or(lookup.server.as_ref().map(|(id, _)| str_value(id))),
        ),
        (
            "program",
            nil_or(
                lookup
                    .server
                    .as_ref()
                    .map(|(_, program)| str_value(to_agent_path(Path::new(program)))),
            ),
        ),
        ("reason", nil_or(lookup.reason.map(str_value))),
    ]))
}

/// `path` relative to `root` when it is absolute and inside it.
fn relative_path(root: &Path, path: &Path) -> String {
    let canonical = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let relative = if path.is_absolute() {
        canonical.strip_prefix(root).unwrap_or(path)
    } else {
        path
    };
    relative.to_string_lossy().replace('\\', "/")
}
