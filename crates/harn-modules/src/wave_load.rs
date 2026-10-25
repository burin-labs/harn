//! Breadth-first wave loading: recall remembered modules, parse the rest.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use harn_lexer::Span;
use harn_parser::{Parser, SNode};

use crate::declarations::{
    collect_callable_declarations, collect_module_info, collect_type_declarations,
};
use crate::package_snapshot::PackageSnapshot;
use crate::{load_memo, normalize_path, source_reads, stdlib, stdlib_module_name};
use crate::{
    ModuleInfo, ModuleLoadError, ModuleObserver, ParsedModuleSource, ParsedSourceRetention,
};

/// Environment override for the graph-build worker-pool size. `1` forces the
/// serial walk; unset defaults to the machine's available parallelism.
pub const MODULE_GRAPH_JOBS_ENV: &str = "HARN_MODULE_GRAPH_JOBS";

type LoadedModule = (Arc<ModuleInfo>, Option<ParsedModuleSource>);

/// Load one BFS wave of module paths. Remembered modules cost a read and a
/// comparison, so they load here; only the rest reach the parse workers.
/// Results are index-aligned with `paths`.
pub(crate) fn load_wave(
    paths: &[PathBuf],
    package_snapshots: &[PackageSnapshot],
    parsed_source_retention: ParsedSourceRetention<'_>,
    source_overrides: Option<&HashMap<PathBuf, String>>,
    observer: Option<&ModuleObserver<'_>>,
) -> Vec<LoadedModule> {
    let mut loaded: Vec<Option<LoadedModule>> = Vec::with_capacity(paths.len());
    let mut pending = Vec::new();
    for path in paths {
        let source = wave_module_source(path, source_overrides);
        // A retained program or an observer needs this load's own syntax tree.
        let memoized = !parsed_source_retention.retains(path) && observer.is_none();
        let remembered = match &source {
            Ok(Some(text)) if memoized => load_memo::recall(path, text, package_snapshots),
            _ => None,
        };
        if remembered.is_none() {
            pending.push((loaded.len(), path, source));
        }
        loaded.push(remembered.map(|module| (module, None)));
    }
    let parsed = parse_wave(
        paths.len(),
        &pending,
        package_snapshots,
        parsed_source_retention,
        observer,
    );
    for ((index, _, _), module) in pending.iter().zip(parsed) {
        loaded[*index] = Some(module);
    }
    loaded
        .into_iter()
        .map(|module| module.expect("every wave path was loaded"))
        .collect()
}

type PendingModule<'a> = (usize, &'a PathBuf, Result<Option<String>, String>);

/// Parse the modules a wave could not recall, index-aligned with `pending`.
///
/// Whether a module parses on a worker still follows the size of its whole
/// wave, as it did before recall: workers carry the runtime parse stack, and a
/// caller thread may not.
fn parse_wave(
    wave_len: usize,
    pending: &[PendingModule<'_>],
    package_snapshots: &[PackageSnapshot],
    parsed_source_retention: ParsedSourceRetention<'_>,
    observer: Option<&ModuleObserver<'_>>,
) -> Vec<LoadedModule> {
    const MIN_PARALLEL_WAVE: usize = 8;
    let load = |(_, path, source): &PendingModule<'_>| {
        load_module(
            path,
            source,
            package_snapshots,
            parsed_source_retention.retains(path),
            observer,
        )
    };
    let configured = std::env::var(MODULE_GRAPH_JOBS_ENV)
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|&jobs| jobs > 0);
    let pool = configured.unwrap_or_else(|| {
        std::thread::available_parallelism()
            .map(std::num::NonZeroUsize::get)
            .unwrap_or(1)
    });
    if pending.is_empty() || pool.min(wave_len) <= 1 || wave_len < MIN_PARALLEL_WAVE {
        return pending.iter().map(load).collect();
    }
    let workers = pool.min(pending.len());
    let next = std::sync::atomic::AtomicUsize::new(0);
    let mut produced: Vec<(usize, LoadedModule)> = harn_parser::runtime_stack::scope(|scope| {
        let handles: Vec<_> = (0..workers)
            .map(|_| {
                scope
                    .spawn_named("harn-module-parse", || {
                        let mut local = Vec::new();
                        loop {
                            let index = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                            let Some(module) = pending.get(index) else {
                                break;
                            };
                            local.push((index, load(module)));
                        }
                        local
                    })
                    .expect("spawn module-parse worker")
            })
            .collect();
        handles
            .into_iter()
            .flat_map(|handle| match handle.join() {
                Ok(local) => local,
                Err(panic) => std::panic::resume_unwind(panic),
            })
            .collect()
    });
    produced.sort_unstable_by_key(|(index, _)| *index);
    produced.into_iter().map(|(_, loaded)| loaded).collect()
}

/// A module's text: the caller's override, the embedded stdlib, or disk.
/// `Ok(None)` is a missing file.
fn wave_module_source(
    path: &Path,
    source_overrides: Option<&HashMap<PathBuf, String>>,
) -> Result<Option<String>, String> {
    match source_overrides.and_then(|overrides| overrides.get(&normalize_path(path))) {
        Some(source) => Ok(Some(source.clone())),
        None => source_reads::read_module_source_checked(path),
    }
}

fn load_module(
    path: &Path,
    source: &Result<Option<String>, String>,
    package_snapshots: &[PackageSnapshot],
    retain_parsed_source: bool,
    observer: Option<&ModuleObserver<'_>>,
) -> LoadedModule {
    let source = match source {
        Ok(Some(source)) => source,
        Ok(None) => return (Arc::default(), None),
        Err(message) => {
            let module = ModuleInfo {
                load_error: Some(ModuleLoadError {
                    message: message.clone(),
                    span: Span::dummy(),
                }),
                ..ModuleInfo::default()
            };
            return (Arc::new(module), None);
        }
    };
    let (module, program) = project_module(path, source, package_snapshots, observer);
    let module = Arc::new(module);
    if !retain_parsed_source && observer.is_none() {
        load_memo::remember(path, source.clone(), &module);
    }
    let parsed = program
        .filter(|_| retain_parsed_source)
        .map(|program| ParsedModuleSource {
            source: source.clone(),
            program,
        });
    (module, parsed)
}

/// Lex, parse, and project one module's source. The program is `None` when
/// lexing or parsing failed; the failure is recorded on the module instead.
fn project_module(
    path: &Path,
    source: &str,
    package_snapshots: &[PackageSnapshot],
    observer: Option<&ModuleObserver<'_>>,
) -> (ModuleInfo, Option<Vec<SNode>>) {
    let failed = |message: String, span: Span| {
        if let Some(observer) = observer {
            observer(path, source, None);
        }
        let module = ModuleInfo {
            load_error: Some(ModuleLoadError { message, span }),
            ..ModuleInfo::default()
        };
        (module, None)
    };
    let tokens = match harn_lexer::Lexer::new(source).tokenize() {
        Ok(tokens) => tokens,
        Err(error) => return failed(error.to_string(), error.span()),
    };
    let program = match Parser::new(tokens).parse() {
        Ok(program) => program,
        Err(error) => return failed(error.to_string(), error.span()),
    };

    if let Some(observer) = observer {
        observer(path, source, Some(&program));
    }
    let mut module = ModuleInfo::default();
    for node in &program {
        collect_module_info(path, node, &mut module, package_snapshots);
        collect_type_declarations(node, &mut module.type_declarations);
        collect_callable_declarations(node, &mut module.callable_declarations);
    }
    if let Some(stdlib_module) = stdlib_module_name(path) {
        module.own_exports.extend(
            stdlib::builtin_reexports(stdlib_module)
                .iter()
                .map(|name| (*name).to_string()),
        );
    }
    // Seed the transitive `exports` set from local exports plus selective
    // re-export names. Wildcard re-exports are folded in by
    // [`resolve_re_exports`] after every module has been loaded.
    module.exports.extend(module.own_exports.iter().cloned());
    module
        .exports
        .extend(module.selective_re_exports.keys().cloned());
    (module, Some(program))
}
