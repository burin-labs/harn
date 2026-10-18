//! Which language server answers for which language, and where its
//! workspace root is.

use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use crate::ast::Language;

/// When a freshly started server can answer semantic queries.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Readiness {
    /// After the `initialized` handshake; requests block server-side until
    /// the project is loaded.
    Initialized,
    /// After rust-analyzer reports `quiescent` through
    /// `experimental/serverStatus`; before that, references are partial.
    Quiescent,
}

/// How to find the workspace root a server should open.
#[derive(Clone, Copy, Debug)]
pub(crate) enum RootRule {
    /// The outermost ancestor holding one of the markers, so a Cargo
    /// workspace member resolves to the workspace.
    Outermost(&'static [&'static str]),
}

pub(crate) struct ServerSpec {
    /// Stable identifier reported in tool results.
    pub id: &'static str,
    /// Candidate executables, first found wins.
    pub programs: &'static [&'static str],
    /// Names the executable instead, e.g.
    /// `HARN_LSP_SERVER_RUST_ANALYZER=/opt/ra/bin/rust-analyzer`. A path that
    /// does not exist forces the fallback, which evals use as a control.
    pub program_env: &'static str,
    pub args: &'static [&'static str],
    pub language_id: fn(Language) -> &'static str,
    pub root: RootRule,
    pub readiness: Readiness,
    initialization: fn() -> Value,
}

impl ServerSpec {
    pub(crate) fn initialization_options(&self) -> Value {
        (self.initialization)()
    }

    /// Answer to every `workspace/configuration` item.
    pub(crate) fn settings(&self) -> Value {
        (self.initialization)()
    }

    pub(crate) fn resolve_program(&self) -> Result<String, String> {
        #[cfg(test)]
        if let Some(program) = test_override::get(self.id) {
            return Ok(program);
        }
        if let Some(program) = std::env::var_os(self.program_env) {
            return Ok(program.to_string_lossy().into_owned());
        }
        self.programs
            .iter()
            .find_map(|program| find_on_path(program))
            .map(|path| path.to_string_lossy().into_owned())
            .ok_or_else(|| {
                format!(
                    "{} is not installed (looked for {})",
                    self.id,
                    self.programs.join(", ")
                )
            })
    }

    pub(crate) fn workspace_root(&self, file: &Path, index_root: &Path) -> Option<PathBuf> {
        let ancestors = || {
            file.ancestors()
                .skip(1)
                .take_while(|dir| dir.starts_with(index_root))
        };
        let marked = |dir: &&Path, markers: &[&str]| markers.iter().any(|m| dir.join(m).exists());
        match self.root {
            RootRule::Outermost(markers) => ancestors()
                .filter(|dir| marked(dir, markers))
                .last()
                .map(Path::to_path_buf),
        }
    }
}

fn find_on_path(program: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .flat_map(|dir| {
            let plain = dir.join(program);
            let exe = dir.join(format!("{program}.exe"));
            [plain, exe]
        })
        .find(|candidate| candidate.is_file())
}

const RUST_ANALYZER: ServerSpec = ServerSpec {
    id: "rust-analyzer",
    programs: &["rust-analyzer"],
    program_env: "HARN_LSP_SERVER_RUST_ANALYZER",
    args: &[],
    language_id: |_| "rust",
    root: RootRule::Outermost(&["Cargo.toml"]),
    readiness: Readiness::Quiescent,
    // Rename needs name resolution only. Build scripts and proc-macro servers
    // mean a `cargo check` across the workspace, and priming every
    // dependency delays quiescence. Occurrences written inside derive and
    // attribute inputs are still found without expanding them.
    initialization: || {
        json!({
            "checkOnSave": false,
            "check": {"workspace": false},
            // Every feature, so feature-gated modules are analyzed too.
            "cargo": {"buildScripts": {"enable": false}, "features": "all"},
            "procMacro": {"enable": false},
            "cachePriming": {"enable": false},
            "diagnostics": {"enable": false},
        })
    },
};

/// The server that renames `language`, if one is registered.
pub(crate) fn for_language(language: Language) -> Option<&'static ServerSpec> {
    match language {
        Language::Rust => Some(&RUST_ANALYZER),
        _ => None,
    }
}

#[cfg(test)]
pub(crate) mod test_override {
    use std::cell::RefCell;
    use std::collections::HashMap;

    thread_local! {
        static PROGRAMS: RefCell<HashMap<&'static str, String>> = RefCell::new(HashMap::new());
    }

    pub(crate) fn get(id: &str) -> Option<String> {
        PROGRAMS.with(|programs| programs.borrow().get(id).cloned())
    }

    /// Point server `id` at `program` for the current test thread.
    pub(crate) fn set(id: &'static str, program: &str) {
        PROGRAMS.with(|programs| programs.borrow_mut().insert(id, program.to_string()));
    }
}
