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
    /// After the work-done progress an opened file starts has ended.
    /// typescript-language-server answers from the opened file alone until
    /// its configured project has loaded.
    ProjectLoaded,
}

/// How to find the workspace root a server should open.
#[derive(Clone, Copy, Debug)]
pub(crate) enum RootRule {
    /// The outermost ancestor holding one of the markers, so a Cargo
    /// workspace member resolves to the workspace.
    Outermost(&'static [&'static str]),
    /// The nearest ancestor holding a marker, else the index root; servers
    /// that infer a project for loose files take this.
    NearestOrIndexRoot(&'static [&'static str]),
}

/// One way to start a server for a language.
pub(crate) struct Launch {
    /// Stable identifier reported in tool results.
    pub id: &'static str,
    /// Candidate executables, first found wins.
    pub programs: &'static [&'static str],
    /// Names the executable instead, e.g.
    /// `HARN_LSP_SERVER_RUST_ANALYZER=/opt/ra/bin/rust-analyzer`. A path that
    /// does not exist forces the fallback, which evals use as a control.
    pub program_env: &'static str,
    pub args: &'static [&'static str],
    /// A project directory searched before `PATH`, for servers a project
    /// pins as a dependency (`node_modules/.bin`).
    pub project_bin: Option<&'static str>,
    pub readiness: Readiness,
}

pub(crate) struct ServerSpec {
    /// Cache key for the language family.
    pub id: &'static str,
    /// Tried in order until one initializes.
    pub launches: &'static [Launch],
    pub language_id: fn(Language) -> &'static str,
    pub root: RootRule,
    initialization: fn() -> Value,
    /// Answer to every `workspace/configuration` item.
    /// Answer to a `workspace/configuration` item for `section`.
    settings: fn(&str) -> Value,
}

impl ServerSpec {
    pub(crate) fn initialization_options(&self) -> Value {
        (self.initialization)()
    }

    pub(crate) fn settings(&self) -> fn(&str) -> Value {
        self.settings
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
            RootRule::NearestOrIndexRoot(markers) => Some(
                ancestors()
                    .find(|dir| marked(dir, markers))
                    .unwrap_or(index_root)
                    .to_path_buf(),
            ),
        }
    }
}

impl Launch {
    pub(crate) fn resolve_program(&self, root: &Path) -> Result<String, String> {
        #[cfg(test)]
        if let Some(program) = test_override::get(self.id) {
            return Ok(program);
        }
        if let Some(program) = std::env::var_os(self.program_env) {
            return Ok(program.to_string_lossy().into_owned());
        }
        let project = self.project_bin.map(|dir| root.join(dir));
        self.programs
            .iter()
            .find_map(|program| {
                project
                    .as_deref()
                    .and_then(|dir| find_in(dir, program))
                    .or_else(|| find_on_path(program))
            })
            .map(|path| path.to_string_lossy().into_owned())
            .ok_or_else(|| {
                format!(
                    "{} is not installed (looked for {})",
                    self.id,
                    self.programs.join(", ")
                )
            })
    }
}

fn find_in(dir: &Path, program: &str) -> Option<PathBuf> {
    [dir.join(program), dir.join(format!("{program}.exe"))]
        .into_iter()
        .find(|candidate| candidate.is_file())
}

fn find_on_path(program: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).find_map(|dir| find_in(&dir, program))
}

const RUST_ANALYZER: ServerSpec = ServerSpec {
    id: "rust",
    launches: &[Launch {
        id: "rust-analyzer",
        programs: &["rust-analyzer"],
        program_env: "HARN_LSP_SERVER_RUST_ANALYZER",
        args: &[],
        project_bin: None,
        readiness: Readiness::Quiescent,
    }],
    language_id: |_| "rust",
    root: RootRule::Outermost(&["Cargo.toml"]),
    initialization: rust_analyzer_settings,
    // rust-analyzer re-reads its settings from `workspace/configuration`.
    settings: |section| match section {
        "rust-analyzer" => rust_analyzer_settings(),
        _ => Value::Null,
    },
};

/// Rename needs name resolution only. Build scripts and proc-macro servers
/// mean a `cargo check` across the workspace, and priming every dependency
/// delays quiescence. Occurrences written inside derive and attribute inputs
/// are still found without expanding them.
fn rust_analyzer_settings() -> Value {
    json!({
        "checkOnSave": false,
        "check": {"workspace": false},
        // Every feature, so feature-gated modules are analyzed too.
        "cargo": {"buildScripts": {"enable": false}, "features": "all"},
        "procMacro": {"enable": false},
        "cachePriming": {"enable": false},
        "diagnostics": {"enable": false},
    })
}

const TYPESCRIPT: ServerSpec = ServerSpec {
    id: "typescript",
    // typescript-language-server drives a TypeScript 6 or older `tsserver`;
    // TypeScript 7 is native and serves LSP from `tsc --lsp`.
    launches: &[
        Launch {
            id: "typescript-language-server",
            programs: &["typescript-language-server"],
            program_env: "HARN_LSP_SERVER_TYPESCRIPT_LANGUAGE_SERVER",
            args: &["--stdio"],
            project_bin: Some("node_modules/.bin"),
            readiness: Readiness::ProjectLoaded,
        },
        Launch {
            id: "tsc-lsp",
            programs: &["tsc"],
            program_env: "HARN_LSP_SERVER_TSC_LSP",
            args: &["--lsp", "--stdio"],
            project_bin: Some("node_modules/.bin"),
            // Loads the project before answering and reports no progress.
            readiness: Readiness::Initialized,
        },
    ],
    language_id: |language| match language {
        Language::Tsx => "typescriptreact",
        Language::JavaScript => "javascript",
        Language::Jsx => "javascriptreact",
        _ => "typescript",
    },
    root: RootRule::NearestOrIndexRoot(&["tsconfig.json", "jsconfig.json", "package.json"]),
    // An agent's rename means the new name everywhere in the workspace, so
    // shorthand properties and re-exports are renamed rather than aliased
    // to their old spelling (`export { TextSpan as Span }`).
    initialization: || {
        json!({"preferences": {
            "providePrefixAndSuffixTextForRename": false,
            "allowRenameOfImportPath": false,
        }})
    },
    // The native TypeScript 7 server reads VS Code-style settings.
    settings: |section| match section {
        "typescript" | "javascript" | "js/ts" => {
            json!({"preferences": {"useAliasesForRenames": false}})
        }
        _ => Value::Null,
    },
};

const PYRIGHT: ServerSpec = ServerSpec {
    id: "python",
    launches: &[Launch {
        id: "pyright",
        programs: &["basedpyright-langserver", "pyright-langserver"],
        program_env: "HARN_LSP_SERVER_PYRIGHT",
        args: &["--stdio"],
        project_bin: None,
        readiness: Readiness::Initialized,
    }],
    language_id: |_| "python",
    root: RootRule::NearestOrIndexRoot(&[
        "pyrightconfig.json",
        "pyproject.toml",
        "setup.py",
        "setup.cfg",
    ]),
    initialization: || Value::Null,
    settings: |_| Value::Null,
};

/// The server that renames `language`, if one is registered.
pub(crate) fn for_language(language: Language) -> Option<&'static ServerSpec> {
    match language {
        Language::Rust => Some(&RUST_ANALYZER),
        Language::TypeScript | Language::Tsx | Language::JavaScript | Language::Jsx => {
            Some(&TYPESCRIPT)
        }
        Language::Python => Some(&PYRIGHT),
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
