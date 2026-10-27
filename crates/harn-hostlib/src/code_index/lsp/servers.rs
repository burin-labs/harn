//! Which language server answers for which language, and where its
//! workspace root is.

use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use crate::ast::Language;

use super::install;

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
    /// The outermost ancestor holding a workspace marker, else the nearest
    /// holding a module marker: a `go.work` spans modules, a `go.mod` is one.
    WorkspaceOrModule {
        workspace: &'static [&'static str],
        module: &'static [&'static str],
    },
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
    /// Give the server a root path with no `.`/`_`-prefixed element.
    pub hidden_path_safe_root: bool,
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
            RootRule::WorkspaceOrModule { workspace, module } => ancestors()
                .filter(|dir| marked(dir, workspace))
                .last()
                .or_else(|| ancestors().find(|dir| marked(dir, module)))
                .map(Path::to_path_buf),
        }
    }
}

impl Launch {
    /// The executable that starts this server for `root`, or why there is none.
    pub(crate) fn resolve_program(&self, root: &Path) -> Result<String, String> {
        #[cfg(test)]
        if let Some(program) = test_override::get(self.id) {
            return Ok(program);
        }
        self.resolve_with(&install::ProcessProbe, root)
    }

    /// Search order: the override variable, the project's own bin directory,
    /// `PATH`, then the install directories `PATH` may omit.
    pub(crate) fn resolve_with(
        &self,
        probe: &dyn install::Probe,
        root: &Path,
    ) -> Result<String, String> {
        if let Some(program) = probe.raw_var(self.program_env) {
            // A set override wins even when it names nothing, so evals can
            // force the fallback.
            let program = PathBuf::from(program);
            return if program.is_file() {
                Ok(program.to_string_lossy().into_owned())
            } else {
                Err(format!(
                    "{} names `{}`, which is not a file",
                    self.program_env,
                    program.display()
                ))
            };
        }
        let mut dirs: Vec<PathBuf> = self
            .project_bin
            .map(|dir| root.join(dir))
            .into_iter()
            .collect();
        if let Some(path) = probe.var("PATH") {
            dirs.extend(std::env::split_paths(&path).filter(|dir| dir.is_absolute()));
        }
        dirs.extend(install::install_dirs(probe, root));
        let mut skipped = Vec::new();
        for program in self.programs {
            for dir in &dirs {
                let Some(found) = find_in(dir, program) else {
                    continue;
                };
                match self.unusable(probe, root, &found, program) {
                    Some(reason) => skipped.push(reason),
                    None => return Ok(found.to_string_lossy().into_owned()),
                }
            }
        }
        skipped.dedup();
        if !skipped.is_empty() {
            return Err(skipped.join("; "));
        }
        Err(format!(
            "{} is not installed (looked for {} on PATH and in {})",
            self.id,
            self.programs.join(", "),
            install::install_dirs(probe, root)
                .iter()
                .map(|dir| dir.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        ))
    }

    /// Why a found executable cannot serve: a rustup proxy whose selected
    /// toolchain lacks the component, or a `tsc` older than TypeScript 7.
    fn unusable(
        &self,
        probe: &dyn install::Probe,
        root: &Path,
        found: &Path,
        program: &str,
    ) -> Option<String> {
        if install::is_rustup_proxy(found) {
            let Some(toolchain) = install::rustup_toolchain(probe, root) else {
                return Some(format!(
                    "`{}` is a rustup proxy, and rustup selects no installed toolchain for {}",
                    found.display(),
                    root.display()
                ));
            };
            return find_in(&toolchain.bin, program).is_none().then(|| {
                format!(
                    "{program} is not installed for the {} toolchain (rustup component add {program})",
                    toolchain.name
                )
            });
        }
        if program == "tsc" {
            let Some(major) = typescript_major(found) else {
                return Some(format!(
                    "cannot determine the TypeScript version for `{}`; `tsc --lsp` needs TypeScript 7",
                    found.display()
                ));
            };
            return (major < 7).then(|| {
                format!(
                    "`{}` is TypeScript {major}; `tsc --lsp` needs TypeScript 7",
                    found.display()
                )
            });
        }
        None
    }
}

/// The major version of the `typescript` package a `tsc` launcher belongs to,
/// when its `package.json` can be found.
fn typescript_major(tsc: &Path) -> Option<u64> {
    let mut script = std::fs::canonicalize(tsc).ok()?;
    if script
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("cmd"))
    {
        // npm's Windows shim launches a relative TypeScript script. Accept
        // only the two package-manager layouts, not an unrelated sibling
        // manifest beside an arbitrary executable.
        let shim = std::fs::read_to_string(&script).ok()?;
        let (_, invocation) = shim
            .lines()
            .last()?
            .rsplit_once(" & \"%_prog%\"  \"%dp0%\\")?;
        let target = invocation.strip_suffix("\" %*")?;
        let relative = match target {
            r"..\typescript\bin\tsc" => "../typescript/bin/tsc",
            r"node_modules\typescript\bin\tsc" => "node_modules/typescript/bin/tsc",
            _ => return None,
        };
        script = std::fs::canonicalize(script.parent()?.join(relative)).ok()?;
    }
    let package = script.parent()?.parent()?.join("package.json");
    let manifest: Value = serde_json::from_str(&std::fs::read_to_string(package).ok()?).ok()?;
    if manifest.get("name")?.as_str()? != "typescript" {
        return None;
    }
    manifest
        .get("version")?
        .as_str()?
        .split('.')
        .next()?
        .parse()
        .ok()
}

#[cfg(test)]
mod version_tests {
    use super::typescript_major;

    #[test]
    fn npm_windows_shims_identify_the_launched_typescript_package() {
        let root = tempfile::tempdir().unwrap();
        for (shim_dir, package_dir, target) in [
            (
                "local/node_modules/.bin",
                "local/node_modules/typescript",
                r"..\typescript\bin\tsc",
            ),
            (
                "global",
                "global/node_modules/typescript",
                r"node_modules\typescript\bin\tsc",
            ),
        ] {
            let shim_dir = root.path().join(shim_dir);
            let package_dir = root.path().join(package_dir);
            std::fs::create_dir_all(&shim_dir).unwrap();
            std::fs::create_dir_all(package_dir.join("bin")).unwrap();
            std::fs::write(package_dir.join("bin/tsc"), "").unwrap();
            std::fs::write(
                package_dir.join("package.json"),
                r#"{"name":"typescript","version":"7.0.1"}"#,
            )
            .unwrap();
            let shim = shim_dir.join("tsc.cmd");
            std::fs::write(&shim, format!("endLocal & goto #_undefined_# 2>NUL || title %COMSPEC% & \"%_prog%\"  \"%dp0%\\{target}\" %*\r\n")).unwrap();
            assert_eq!(typescript_major(&shim), Some(7));
            std::fs::write(&shim, "@echo off\r\necho unrelated\r\n").unwrap();
            assert_eq!(
                typescript_major(&shim),
                None,
                "an unrelated launcher must not inherit a neighboring package's version"
            );
        }
    }
}

/// The executable `program` names in `dir`. Windows launches by extension, and
/// npm installs its global shims there as `.cmd` files.
fn find_in(dir: &Path, program: &str) -> Option<PathBuf> {
    let names: Vec<String> = if cfg!(windows) {
        ["exe", "cmd", "bat"]
            .iter()
            .map(|extension| format!("{program}.{extension}"))
            .collect()
    } else {
        vec![program.to_string()]
    };
    names
        .into_iter()
        .map(|name| dir.join(name))
        .find(|candidate| candidate.is_file())
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
        hidden_path_safe_root: false,
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
            hidden_path_safe_root: false,
        },
        Launch {
            id: "tsc-lsp",
            programs: &["tsc"],
            program_env: "HARN_LSP_SERVER_TSC_LSP",
            args: &["--lsp", "--stdio"],
            project_bin: Some("node_modules/.bin"),
            // Loads the project before answering and reports no progress.
            readiness: Readiness::Initialized,
            hidden_path_safe_root: false,
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
    //
    // With its default syntax server, typescript-language-server routes
    // definition, references and rename to a syntax-only tsserver while the
    // project loads, which answers without types (`map.get` resolves to
    // nothing). Reopening documents starts a load, so answers raced it. The
    // semantic server alone blocks until the project is loaded.
    initialization: || {
        json!({
            "preferences": {
                "providePrefixAndSuffixTextForRename": false,
                "allowRenameOfImportPath": false,
            },
            "tsserver": {"useSyntaxServer": "never"},
        })
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
        hidden_path_safe_root: false,
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

const GOPLS: ServerSpec = ServerSpec {
    id: "go",
    launches: &[Launch {
        id: "gopls",
        programs: &["gopls"],
        program_env: "HARN_LSP_SERVER_GOPLS",
        args: &[],
        project_bin: None,
        // gopls reports package loading as work-done progress and answers
        // from partial metadata until it ends.
        readiness: Readiness::ProjectLoaded,
        hidden_path_safe_root: true,
    }],
    language_id: |_| "go",
    root: RootRule::WorkspaceOrModule {
        workspace: &["go.work"],
        module: &["go.mod"],
    },
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
        Language::Go => Some(&GOPLS),
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
