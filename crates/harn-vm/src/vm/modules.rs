use std::cell::RefCell;
use std::collections::{BTreeMap, HashSet};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::rc::Rc;

use serde::Deserialize;

use crate::value::{ModuleFunctionRegistry, VmClosure, VmEnv, VmError, VmValue};

use super::{ScopeSpan, Vm};

#[derive(Clone)]
pub(crate) struct LoadedModule {
    pub(crate) functions: BTreeMap<String, Rc<VmClosure>>,
    pub(crate) public_names: HashSet<String>,
}

#[derive(Debug, Default, Deserialize)]
struct PackageManifest {
    #[serde(default)]
    exports: BTreeMap<String, String>,
}

fn resolve_package_import(base: &Path, import_path: &str) -> Option<PathBuf> {
    for anchor in base.ancestors() {
        let packages_root = anchor.join(".harn/packages");
        if !packages_root.is_dir() {
            if anchor.join(".git").exists() {
                break;
            }
            continue;
        }
        if let Some(path) = resolve_from_packages_root(&packages_root, import_path) {
            return Some(path);
        }
        if anchor.join(".git").exists() {
            break;
        }
    }
    None
}

fn resolve_from_packages_root(packages_root: &Path, import_path: &str) -> Option<PathBuf> {
    let pkg_path = packages_root.join(import_path);
    if let Some(path) = finalize_package_target(&pkg_path) {
        return Some(path);
    }

    let (package_name, export_name) = import_path.split_once('/')?;
    let manifest_path = packages_root.join(package_name).join("harn.toml");
    let manifest = read_package_manifest(&manifest_path)?;
    let rel_path = manifest.exports.get(export_name)?;
    finalize_package_target(&packages_root.join(package_name).join(rel_path))
}

fn read_package_manifest(path: &Path) -> Option<PackageManifest> {
    let content = std::fs::read_to_string(path).ok()?;
    toml::from_str::<PackageManifest>(&content).ok()
}

fn finalize_package_target(path: &Path) -> Option<PathBuf> {
    if path.is_dir() {
        let lib = path.join("lib.harn");
        if lib.exists() {
            return Some(lib);
        }
        return Some(path.to_path_buf());
    }
    if path.exists() {
        return Some(path.to_path_buf());
    }
    if path.extension().is_none() {
        let mut with_ext = path.to_path_buf();
        with_ext.set_extension("harn");
        if with_ext.exists() {
            return Some(with_ext);
        }
    }
    None
}

impl Vm {
    fn export_loaded_module(
        &mut self,
        module_path: &Path,
        loaded: &LoadedModule,
        selected_names: Option<&[String]>,
    ) -> Result<(), VmError> {
        let export_names: Vec<String> = if let Some(names) = selected_names {
            names.to_vec()
        } else if !loaded.public_names.is_empty() {
            loaded.public_names.iter().cloned().collect()
        } else {
            loaded.functions.keys().cloned().collect()
        };

        let module_name = module_path.display().to_string();
        for name in export_names {
            let Some(closure) = loaded.functions.get(&name) else {
                return Err(VmError::Runtime(format!(
                    "Import error: '{name}' is not defined in {module_name}"
                )));
            };
            if let Some(VmValue::Closure(_)) = self.env.get(&name) {
                return Err(VmError::Runtime(format!(
                    "Import collision: '{name}' is already defined when importing {module_name}. \
                     Use selective imports to disambiguate: import {{ {name} }} from \"...\""
                )));
            }
            self.env
                .define(&name, VmValue::Closure(Rc::clone(closure)), false)?;
        }
        Ok(())
    }

    /// Execute an import, reading and running the file's declarations.
    pub(super) fn execute_import<'a>(
        &'a mut self,
        path: &'a str,
        selected_names: Option<&'a [String]>,
    ) -> Pin<Box<dyn Future<Output = Result<(), VmError>> + 'a>> {
        Box::pin(async move {
            let _import_span = ScopeSpan::new(crate::tracing::SpanKind::Import, path.to_string());

            if let Some(module) = path.strip_prefix("std/") {
                if let Some(source) = crate::stdlib_modules::get_stdlib_source(module) {
                    let synthetic = PathBuf::from(format!("<stdlib>/{module}.harn"));
                    if self.imported_paths.contains(&synthetic) {
                        return Ok(());
                    }
                    if let Some(loaded) = self.module_cache.get(&synthetic).cloned() {
                        return self.export_loaded_module(&synthetic, &loaded, selected_names);
                    }
                    self.imported_paths.push(synthetic.clone());

                    let mut lexer = harn_lexer::Lexer::new(source);
                    let tokens = lexer.tokenize().map_err(|e| {
                        VmError::Runtime(format!("stdlib lex error in std/{module}: {e}"))
                    })?;
                    let mut parser = harn_parser::Parser::new(tokens);
                    let program = parser.parse().map_err(|e| {
                        VmError::Runtime(format!("stdlib parse error in std/{module}: {e}"))
                    })?;

                    let loaded = self.import_declarations(&program, None).await?;
                    self.imported_paths.pop();
                    self.module_cache.insert(synthetic.clone(), loaded.clone());
                    self.export_loaded_module(&synthetic, &loaded, selected_names)?;
                    return Ok(());
                }
                return Err(VmError::Runtime(format!(
                    "Unknown stdlib module: std/{module}"
                )));
            }

            let base = self
                .source_dir
                .clone()
                .unwrap_or_else(|| PathBuf::from("."));
            let mut file_path = base.join(path);

            if !file_path.exists() && file_path.extension().is_none() {
                file_path.set_extension("harn");
            }

            if !file_path.exists() {
                if let Some(resolved) = resolve_package_import(&base, path) {
                    file_path = resolved;
                }
            }

            let canonical = file_path
                .canonicalize()
                .unwrap_or_else(|_| file_path.clone());
            if self.imported_paths.contains(&canonical) {
                return Ok(());
            }
            if let Some(loaded) = self.module_cache.get(&canonical).cloned() {
                return self.export_loaded_module(&canonical, &loaded, selected_names);
            }
            self.imported_paths.push(canonical.clone());

            let source = std::fs::read_to_string(&file_path).map_err(|e| {
                VmError::Runtime(format!(
                    "Import error: cannot read '{}': {e}",
                    file_path.display()
                ))
            })?;

            let mut lexer = harn_lexer::Lexer::new(&source);
            let tokens = lexer
                .tokenize()
                .map_err(|e| VmError::Runtime(format!("Import lex error: {e}")))?;
            let mut parser = harn_parser::Parser::new(tokens);
            let program = parser
                .parse()
                .map_err(|e| VmError::Runtime(format!("Import parse error: {e}")))?;

            let loaded = self.import_declarations(&program, Some(&file_path)).await?;
            self.imported_paths.pop();
            self.module_cache.insert(canonical.clone(), loaded.clone());
            self.export_loaded_module(&canonical, &loaded, selected_names)?;

            Ok(())
        })
    }

    /// Process top-level declarations from an imported module.
    fn import_declarations<'a>(
        &'a mut self,
        program: &'a [harn_parser::SNode],
        file_path: Option<&'a Path>,
    ) -> Pin<Box<dyn Future<Output = Result<LoadedModule, VmError>> + 'a>> {
        Box::pin(async move {
            let caller_env = self.env.clone();
            let old_source_dir = self.source_dir.clone();
            self.env = VmEnv::new();
            if let Some(fp) = file_path {
                if let Some(parent) = fp.parent() {
                    self.source_dir = Some(parent.to_path_buf());
                }
            }

            for node in program {
                match &node.node {
                    harn_parser::Node::ImportDecl { path: sub_path } => {
                        self.execute_import(sub_path, None).await?;
                    }
                    harn_parser::Node::SelectiveImport {
                        names,
                        path: sub_path,
                    } => {
                        self.execute_import(sub_path, Some(names)).await?;
                    }
                    _ => {}
                }
            }

            // Route top-level `var`/`let` bindings into a shared
            // `module_state` rather than `module_env`. If they appeared in
            // `module_env` (captured by each closure's lexical snapshot),
            // every call's per-invocation env clone would shadow them and
            // writes would land in a per-call copy discarded on return.
            let module_state: crate::value::ModuleState = {
                let mut init_env = self.env.clone();
                let init_nodes: Vec<harn_parser::SNode> = program
                    .iter()
                    .filter(|sn| {
                        matches!(
                            &sn.node,
                            harn_parser::Node::VarBinding { .. }
                                | harn_parser::Node::LetBinding { .. }
                        )
                    })
                    .cloned()
                    .collect();
                if !init_nodes.is_empty() {
                    let init_compiler = crate::Compiler::new();
                    let init_chunk = init_compiler
                        .compile(&init_nodes)
                        .map_err(|e| VmError::Runtime(format!("Import init compile error: {e}")))?;
                    // Save frame state so run_chunk_entry's top-level
                    // frame-pop doesn't restore self.env.
                    let saved_env = std::mem::replace(&mut self.env, init_env);
                    let saved_frames = std::mem::take(&mut self.frames);
                    let saved_handlers = std::mem::take(&mut self.exception_handlers);
                    let saved_iterators = std::mem::take(&mut self.iterators);
                    let saved_deadlines = std::mem::take(&mut self.deadlines);
                    let init_result = self.run_chunk(&init_chunk).await;
                    init_env = std::mem::replace(&mut self.env, saved_env);
                    self.frames = saved_frames;
                    self.exception_handlers = saved_handlers;
                    self.iterators = saved_iterators;
                    self.deadlines = saved_deadlines;
                    init_result?;
                }
                Rc::new(RefCell::new(init_env))
            };

            let module_env = self.env.clone();
            let registry: ModuleFunctionRegistry = Rc::new(RefCell::new(BTreeMap::new()));
            let source_dir = file_path.and_then(|fp| fp.parent().map(|p| p.to_path_buf()));
            let mut functions: BTreeMap<String, Rc<VmClosure>> = BTreeMap::new();
            let mut public_names: HashSet<String> = HashSet::new();

            for node in program {
                // Imports may carry `@deprecated` / `@test` etc. on top-level
                // fn decls; transparently peel the wrapper before pattern
                // matching the FnDecl shape.
                let inner = match &node.node {
                    harn_parser::Node::AttributedDecl { inner, .. } => inner.as_ref(),
                    _ => node,
                };
                let harn_parser::Node::FnDecl {
                    name,
                    params,
                    body,
                    is_pub,
                    ..
                } = &inner.node
                else {
                    continue;
                };

                let mut compiler = crate::Compiler::new();
                let module_source_file = file_path.map(|p| p.display().to_string());
                let func_chunk = compiler
                    .compile_fn_body(params, body, module_source_file)
                    .map_err(|e| VmError::Runtime(format!("Import compile error: {e}")))?;
                let closure = Rc::new(VmClosure {
                    func: func_chunk,
                    env: module_env.clone(),
                    source_dir: source_dir.clone(),
                    module_functions: Some(Rc::clone(&registry)),
                    module_state: Some(Rc::clone(&module_state)),
                });
                registry
                    .borrow_mut()
                    .insert(name.clone(), Rc::clone(&closure));
                self.env
                    .define(name, VmValue::Closure(Rc::clone(&closure)), false)?;
                // Publish into module_state so sibling fns can be read
                // as VALUES (e.g. `{handler: other_fn}` or as callbacks).
                // Closures captured module_env BEFORE fn decls were added,
                // so their static env alone can't resolve sibling fns.
                // Direct calls use the module_functions late-binding path;
                // value reads rely on this module_state entry.
                module_state.borrow_mut().define(
                    name,
                    VmValue::Closure(Rc::clone(&closure)),
                    false,
                )?;
                functions.insert(name.clone(), Rc::clone(&closure));
                if *is_pub {
                    public_names.insert(name.clone());
                }
            }

            self.env = caller_env;
            self.source_dir = old_source_dir;

            Ok(LoadedModule {
                functions,
                public_names,
            })
        })
    }

    /// Load a module file and return the exported function closures that
    /// would be visible to a wildcard import.
    pub async fn load_module_exports(
        &mut self,
        path: &Path,
    ) -> Result<BTreeMap<String, Rc<VmClosure>>, VmError> {
        let path_str = path.to_string_lossy().into_owned();
        self.execute_import(&path_str, None).await?;

        let mut file_path = if path.is_absolute() {
            path.to_path_buf()
        } else {
            self.source_dir
                .clone()
                .unwrap_or_else(|| PathBuf::from("."))
                .join(path)
        };
        if !file_path.exists() && file_path.extension().is_none() {
            file_path.set_extension("harn");
        }

        let canonical = file_path
            .canonicalize()
            .unwrap_or_else(|_| file_path.clone());
        let loaded = self.module_cache.get(&canonical).cloned().ok_or_else(|| {
            VmError::Runtime(format!(
                "Import error: failed to cache loaded module '{}'",
                canonical.display()
            ))
        })?;

        let export_names: Vec<String> = if loaded.public_names.is_empty() {
            loaded.functions.keys().cloned().collect()
        } else {
            loaded.public_names.iter().cloned().collect()
        };

        let mut exports = BTreeMap::new();
        for name in export_names {
            let Some(closure) = loaded.functions.get(&name) else {
                return Err(VmError::Runtime(format!(
                    "Import error: exported function '{name}' is missing from {}",
                    canonical.display()
                )));
            };
            exports.insert(name, Rc::clone(closure));
        }

        Ok(exports)
    }
}
