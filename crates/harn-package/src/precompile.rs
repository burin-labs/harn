//! Adjacent bytecode for `.harn` sources, compiled by the binary that links
//! this crate.
//!
//! Every artifact key carries the linked builtin contract, so only a binary
//! with the same harn-vm families as the one that will load the artifacts can
//! produce artifacts it accepts. `harn precompile` projects this module for the
//! CLI's own contract; a host with a leaner VM calls it from its own binary.

use std::path::{Path, PathBuf};

use harn_parser::DiagnosticSeverity;
use harn_vm::bytecode_cache::{
    CacheKey, PrecompiledInterface, CACHE_EXTENSION, INTERFACE_CACHE_EXTENSION,
    MODULE_CACHE_EXTENSION,
};
use harn_vm::module_artifact::{ModuleArtifact, ModuleCompilationContext, ModuleProvenance};

use crate::package;

mod interface;

const GENERATED_SOURCE_WALK_DIRS: &[&str] = &[
    ".burin",
    ".build",
    ".claude",
    ".codex",
    ".git",
    ".next",
    ".svelte-kit",
    ".turbo",
    ".venv",
    "build",
    "coverage",
    "dist",
    "node_modules",
    "target",
];

pub fn should_skip_recursive_source_dir(dir: &Path) -> bool {
    let Some(name) = dir.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    GENERATED_SOURCE_WALK_DIRS.contains(&name)
        || crate::path_policy::is_harn_internal_entry(
            name,
            crate::path_policy::PathEntryKind::Directory,
        )
}

pub fn should_skip_recursive_source_file(file: &Path) -> bool {
    let Some(name) = file.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    // Harn can emit generated source snapshots such as `.harn-eval-*.harn`.
    // This source-file rule is separate from directory-owned runtime state.
    name.starts_with(".harn-")
}

/// Every `.harn` source under `dir`, in path order, skipping generated and
/// runtime-state directories. A sibling `<name>.conformance-skip` marker parks
/// a test tracking a known regression so the suites stay green meanwhile.
pub fn collect_harn_files(dir: &Path, out: &mut Vec<PathBuf>) {
    if let Ok(entries) = std::fs::read_dir(dir) {
        let mut entries: Vec<_> = entries.filter_map(|e| e.ok()).collect();
        entries.sort_by_key(|e| e.path());
        for entry in entries {
            let path = entry.path();
            if path.is_dir() {
                if should_skip_recursive_source_dir(&path) {
                    continue;
                }
                collect_harn_files(&path, out);
            } else if should_skip_recursive_source_file(&path) {
                continue;
            } else if path.extension().is_some_and(|ext| ext == "harn") {
                let skip_marker = path.with_extension("conformance-skip");
                if skip_marker.exists() {
                    continue;
                }
                out.push(path);
            }
        }
    }
}

/// Manifest-derived authority shared by every compilation product for one
/// source file.
///
/// Resolving this once keeps the typechecker, entry chunk, and importable
/// module on the same authority boundary.
#[derive(Clone, Copy)]
pub struct SourceCompilerAuthority {
    trusted_host_dispatch: bool,
}

impl SourceCompilerAuthority {
    pub fn for_source(path: &Path) -> Self {
        Self {
            trusted_host_dispatch: trusted_host_dispatch_for_source(path),
        }
    }

    /// Ordinary authority, whatever manifest is nearby.
    pub fn standalone() -> Self {
        Self {
            trusted_host_dispatch: false,
        }
    }

    pub fn typechecker(self) -> harn_parser::TypeChecker {
        harn_parser::TypeChecker::new().with_privileged_wire_builtins(self.trusted_host_dispatch)
    }

    /// The authority a module compiled under this projection carries.
    ///
    /// Precompiled artifacts are written next to their source, where they are
    /// offered to whoever imports that file. Stamping the authority into their
    /// cache identity is what stops an ordinary import from accepting one
    /// compiled with privileged-wire access.
    pub fn module_provenance(self) -> ModuleProvenance {
        if self.trusted_host_dispatch {
            ModuleProvenance::TrustedHostDispatch
        } else {
            ModuleProvenance::User
        }
    }

    pub fn compiler_with_imported_symbols(
        self,
        context: &ModuleCompilationContext,
    ) -> harn_vm::Compiler {
        let compiler = if self.trusted_host_dispatch {
            harn_vm::Compiler::new_trusted_host_dispatch()
        } else {
            harn_vm::Compiler::new()
        };
        context.configure_compiler(compiler)
    }

    pub fn compile_module_with_imported_symbols(
        self,
        source_path: &Path,
        source: &str,
        context: &ModuleCompilationContext,
    ) -> Result<ModuleArtifact, harn_vm::VmError> {
        if self.trusted_host_dispatch {
            harn_vm::module_artifact::compile_trusted_host_dispatch_module_artifact_from_source_with_context(
                source_path,
                source,
                context,
            )
        } else {
            harn_vm::module_artifact::compile_module_artifact_from_source_with_context(
                source_path,
                source,
                context,
            )
        }
    }
}

/// Read the project's own `[check].trusted_host_dispatch` declaration for the
/// manifest that owns `path`.
pub fn trusted_host_dispatch_for_source(path: &Path) -> bool {
    // A relative path's ancestors run out before the repo-root manifest.
    let absolute = path
        .canonicalize()
        .unwrap_or_else(|_| std::env::current_dir().unwrap_or_default().join(path));
    package::load_check_config(Some(&absolute)).trusted_host_dispatch
}

/// What one source cost this invocation.
///
/// `Reused` did no compile work; a reuse count is the only direct evidence the
/// reuse path was reached at all.
pub enum Outcome {
    Compiled(PathBuf),
    Reused(PathBuf),
}

impl Outcome {
    pub fn destination(&self) -> &PathBuf {
        match self {
            Outcome::Compiled(path) | Outcome::Reused(path) => path,
        }
    }
}

/// One source's outcome and the non-fatal type diagnostics it reported.
pub struct Precompiled {
    pub outcome: Outcome,
    pub warnings: String,
}

/// Compile one source's entry chunk, module artifact and imported interface,
/// or reuse them when the artifacts on disk already carry their keys.
///
/// `source_root` makes the entry key relocatable relative to that root.
/// `out_root` mirrors the tree there instead of writing beside each source.
pub fn precompile_source(
    source_path: &Path,
    source_root: Option<&Path>,
    out_root: Option<&Path>,
) -> Result<Precompiled, String> {
    let source = std::fs::read_to_string(source_path).map_err(|e| format!("read: {e}"))?;
    let path_str = source_path.to_string_lossy();

    // Both the parse and the import resolution the keys come from need it.
    harn_parser::install_builtin_manifest(harn_vm::stdlib::all_builtin_manifest());
    let authority = SourceCompilerAuthority::for_source(source_path);

    let interface_dest = output_path(
        source_path,
        source_root,
        out_root,
        INTERFACE_CACHE_EXTENSION,
    )?;
    let (keys, interface) = artifact_keys(
        source_path,
        &source,
        source_root,
        &authority,
        &interface_dest,
    )?;
    let entry_dest = output_path(source_path, source_root, out_root, CACHE_EXTENSION)?;
    let module_dest = output_path(source_path, source_root, out_root, MODULE_CACHE_EXTENSION)?;
    if keys.match_artifacts_at(&entry_dest, &module_dest) {
        interface
            .store()
            .map_err(|e| format!("write {}: {e}", interface_dest.display()))?;
        return Ok(Precompiled {
            outcome: Outcome::Reused(entry_dest),
            warnings: String::new(),
        });
    }

    let program = harn_parser::parse_source(&source).map_err(|e| format!("parse error: {e}"))?;

    // Resolve imports so a call to an imported symbol that shadows a builtin
    // is checked against the right signature. The interface's graph was built
    // from this same source text.
    let built;
    let graph = match interface.module_graph() {
        Some(graph) => graph,
        None => {
            built = harn_modules::build_with_source(source_path, &source);
            &built
        }
    };
    let checker = graph
        .typecheck_import_config_for_file(source_path)
        .apply_imports_to(authority.typechecker());

    let mut had_type_error = false;
    let mut warnings = String::new();
    let reported = checker.check_with_source(&program, &source).into_iter();
    for diag in reported.filter(|diag| diag.severity.reported_when_executing()) {
        had_type_error |= matches!(diag.severity, DiagnosticSeverity::Error);
        warnings.push_str(&harn_parser::diagnostic::render_type_diagnostic(
            &source, &path_str, &diag,
        ));
    }
    if had_type_error {
        return Err(format!("type errors:\n{warnings}"));
    }

    let entry_chunk = authority
        .compiler_with_imported_symbols(interface.context())
        .compile(&program)
        .map_err(|e| format!("compile error: {e}"))?;
    // A source whose module view does not compile is still a runnable entry.
    let module_artifact = authority
        .compile_module_with_imported_symbols(source_path, &source, interface.context())
        .ok();

    interface
        .store()
        .map_err(|e| format!("write {}: {e}", interface_dest.display()))?;
    harn_vm::bytecode_cache::store_at(&entry_dest, &keys.entry, &entry_chunk)
        .map_err(|e| format!("write {}: {e}", entry_dest.display()))?;
    if let Some(module_artifact) = &module_artifact {
        harn_vm::bytecode_cache::store_module_at(&module_dest, &keys.module, module_artifact)
            .map_err(|e| format!("write {}: {e}", module_dest.display()))?;
    }

    Ok(Precompiled {
        outcome: Outcome::Compiled(entry_dest),
        warnings,
    })
}

/// Counts for one [`precompile_tree`] call, with each failed source's error.
#[derive(Default)]
pub struct TreeReport {
    pub compiled: usize,
    pub reused: usize,
    pub failed: Vec<(PathBuf, String)>,
}

/// Precompile every source under `root` beside itself, with relocatable entry
/// keys, in this process. Keeps going past a failed source.
///
/// Call `harn_stdlib::install()` first and keep `harn_vm::stdlib::force_link()`
/// reachable from `main`: an unlinked builtin slice keys every artifact wrong.
pub fn precompile_tree(root: &Path) -> TreeReport {
    let mut sources = Vec::new();
    collect_harn_files(root, &mut sources);
    let root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    let mut report = TreeReport::default();
    for source in sources {
        match precompile_source(&source, Some(&root), None) {
            Ok(Precompiled {
                outcome: Outcome::Compiled(_),
                ..
            }) => report.compiled += 1,
            Ok(Precompiled {
                outcome: Outcome::Reused(_),
                ..
            }) => report.reused += 1,
            Err(error) => report.failed.push((source, error)),
        }
    }
    report
}

/// The keys the two artifacts for one source will be stored under.
struct ArtifactKeys {
    entry: CacheKey,
    module: CacheKey,
}

impl ArtifactKeys {
    /// True when both artifacts already on disk carry exactly these keys,
    /// judged by the same header comparison a cache load uses. A source whose
    /// module view does not compile has no module artifact, and "absent" and
    /// "stale" look the same from here, so it recompiles.
    fn match_artifacts_at(&self, entry_dest: &Path, module_dest: &Path) -> bool {
        harn_vm::bytecode_cache::entry_artifact_at_matches(entry_dest, &self.entry)
            && harn_vm::bytecode_cache::module_artifact_at_matches(module_dest, &self.module)
    }
}

/// The published package generation `anchor` resolves through, read from its
/// pointer file without leasing it. Equal reads bracket a window in which no
/// replacement generation was published.
fn current_package_generation(anchor: &Path) -> Option<(PathBuf, String)> {
    let root = harn_modules::package_snapshot::PackageSnapshot::nearest_project_root(anchor)?;
    let pointer = harn_modules::package_snapshot::package_current_path(&root);
    Some((root, std::fs::read_to_string(pointer).ok()?))
}

/// Derive both bytecode keys from the prepared imported interface.
fn artifact_keys(
    source_path: &Path,
    source: &str,
    source_root: Option<&Path>,
    authority: &SourceCompilerAuthority,
    interface_dest: &Path,
) -> Result<(ArtifactKeys, PrecompiledInterface), String> {
    let interface =
        interface::prepare_consistent_interface(source_path, source, interface_dest, || {})?;
    let entry = if source_root.is_some() {
        CacheKey::from_relocatable_source(source_path, source)
    } else {
        CacheKey::from_source(source_path, source)
    };
    let module_source = harn_vm::module_source::ModuleSource::from_text(source);
    let module = CacheKey::from_module_source(
        &module_source,
        interface.context(),
        authority.module_provenance(),
    );
    Ok((ArtifactKeys { entry, module }, interface))
}

/// Map a source path under `source_root` to its artifact destination.
///
/// Adjacent artifacts move with their source. An explicit output root mirrors
/// the source tree so equally named modules do not collide.
fn output_path(
    source_path: &Path,
    source_root: Option<&Path>,
    out_root: Option<&Path>,
    extension: &str,
) -> Result<PathBuf, String> {
    source_path
        .file_stem()
        .ok_or_else(|| format!("source has no file stem: {}", source_path.display()))?;
    let Some(out_root) = out_root else {
        return Ok(source_path.with_extension(extension));
    };
    let relative = match source_root {
        Some(root) => {
            let canonical = source_path
                .canonicalize()
                .unwrap_or_else(|_| source_path.to_path_buf());
            canonical
                .strip_prefix(root)
                .map(Path::to_path_buf)
                .unwrap_or_else(|_| {
                    PathBuf::from(source_path.file_name().unwrap_or(source_path.as_os_str()))
                })
        }
        None => PathBuf::from(
            source_path
                .file_name()
                .ok_or_else(|| format!("source has no file name: {}", source_path.display()))?,
        ),
    };
    let mut dest = out_root.join(&relative);
    dest.set_extension(extension);
    Ok(dest)
}
