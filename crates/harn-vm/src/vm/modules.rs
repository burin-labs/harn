use std::collections::{BTreeMap, HashSet};
use std::future::Future;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::{Arc, Mutex, OnceLock};

use crate::bytecode_cache;
use crate::chunk::{Chunk, CompiledFunction};
use crate::module_artifact::{compile_module_artifact_from_source, ModuleArtifact};
use crate::value::{ModuleFunctionRegistry, VmClosure, VmEnv, VmError, VmValue};

use super::{ScopeSpan, Vm};

static STDLIB_MODULE_ARTIFACT_CACHE: OnceLock<Mutex<BTreeMap<String, Arc<ModuleArtifact>>>> =
    OnceLock::new();

fn stdlib_module_artifact_cache() -> &'static Mutex<BTreeMap<String, Arc<ModuleArtifact>>> {
    STDLIB_MODULE_ARTIFACT_CACHE.get_or_init(|| Mutex::new(BTreeMap::new()))
}

#[cfg(test)]
fn reset_stdlib_module_artifact_cache() {
    stdlib_module_artifact_cache().lock().unwrap().clear();
}

#[cfg(test)]
fn stdlib_module_artifact_cache_ptr(module: &str, source: &str) -> Option<usize> {
    let key = stdlib_artifact_cache_key(module, source);
    stdlib_module_artifact_cache()
        .lock()
        .unwrap()
        .get(&key)
        .map(|artifact| Arc::as_ptr(artifact) as usize)
}

#[derive(Clone)]
pub(crate) struct LoadedModule {
    pub(crate) functions: BTreeMap<String, Arc<VmClosure>>,
    pub(crate) public_names: HashSet<String>,
    pub(crate) _module_functions: crate::value::ModuleFunctionRegistry,
    pub(crate) _module_state: crate::value::ModuleState,
}

pub fn resolve_module_import_path(base: &Path, path: &str) -> PathBuf {
    let synthetic_current_file = base.join("__harn_import_base__.harn");
    if let Some(resolved) = harn_modules::resolve_import_path(&synthetic_current_file, path) {
        return resolved;
    }

    let mut file_path = base.join(path);

    if !file_path.exists() && file_path.extension().is_none() {
        file_path.set_extension("harn");
    }

    file_path
}

fn stdlib_artifact_cache_key(module: &str, source: &str) -> String {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    module.hash(&mut hasher);
    source.hash(&mut hasher);
    format!("{module}:{:016x}", hasher.finish())
}

fn stdlib_module_artifact(
    module: &str,
    synthetic: &Path,
    source: &'static str,
) -> Result<Arc<ModuleArtifact>, VmError> {
    let key = stdlib_artifact_cache_key(module, source);
    {
        let cache = stdlib_module_artifact_cache().lock().unwrap();
        if let Some(cached) = cache.get(&key) {
            return Ok(Arc::clone(cached));
        }
    }

    // Stdlib modules are embedded in the binary so their content cannot
    // legitimately change between processes; that means the disk cache
    // for stdlib can use a synthetic source_path. The harn_version field
    // of the cache key gates correctness across releases.
    let lookup = bytecode_cache::load_module(synthetic, source);
    let artifact = if let Some(artifact) = lookup.artifact {
        artifact
    } else {
        let compiled = compile_module_artifact_from_source(synthetic, source)?;
        if let Err(err) = bytecode_cache::store_module(&lookup.key, &compiled) {
            if std::env::var_os("HARN_BYTECODE_CACHE_DEBUG").is_some() {
                eprintln!("[harn] stdlib module cache write skipped for {module}: {err}");
            }
        }
        compiled
    };

    let compiled = Arc::new(artifact);
    let mut cache = stdlib_module_artifact_cache().lock().unwrap();
    if let Some(cached) = cache.get(&key) {
        return Ok(Arc::clone(cached));
    }
    cache.insert(key, Arc::clone(&compiled));
    Ok(compiled)
}

impl Vm {
    async fn load_module_from_source(
        &mut self,
        synthetic: PathBuf,
        source: &str,
    ) -> Result<LoadedModule, VmError> {
        if let Some(loaded) = self.module_cache.get(&synthetic).cloned() {
            return Ok(loaded);
        }
        Arc::make_mut(&mut self.source_cache).insert(synthetic.clone(), source.to_string());

        let artifact = compile_module_artifact_from_source(&synthetic, source)?;

        self.imported_paths.push(synthetic.clone());
        let loaded = self.instantiate_module(None, &artifact).await?;
        self.imported_paths.pop();
        Arc::make_mut(&mut self.module_cache).insert(synthetic, loaded.clone());
        Ok(loaded)
    }

    async fn load_stdlib_module_from_source(
        &mut self,
        module: &str,
        synthetic: PathBuf,
        source: &'static str,
    ) -> Result<LoadedModule, VmError> {
        if let Some(loaded) = self.module_cache.get(&synthetic).cloned() {
            return Ok(loaded);
        }
        Arc::make_mut(&mut self.source_cache).insert(synthetic.clone(), source.to_string());

        let artifact = stdlib_module_artifact(module, &synthetic, source)?;
        self.imported_paths.push(synthetic.clone());
        let loaded = self.instantiate_stdlib_module(artifact.as_ref()).await?;
        self.imported_paths.pop();
        Arc::make_mut(&mut self.module_cache).insert(synthetic, loaded.clone());
        Ok(loaded)
    }

    async fn instantiate_stdlib_module(
        &mut self,
        artifact: &ModuleArtifact,
    ) -> Result<LoadedModule, VmError> {
        self.instantiate_module(None, artifact).await
    }

    /// Instantiate a previously-compiled [`ModuleArtifact`] into a
    /// [`LoadedModule`]. Re-runs nested imports, replays the init chunk
    /// into a fresh module env, mints a [`VmClosure`] for each compiled
    /// function (stamped with `module_source_dir` so imports from inside
    /// those functions resolve against the originating file), and
    /// applies the re-export pass. Used by both stdlib and user-import
    /// code paths.
    async fn instantiate_module(
        &mut self,
        module_source_dir: Option<PathBuf>,
        artifact: &ModuleArtifact,
    ) -> Result<LoadedModule, VmError> {
        let caller_env = self.env.clone();
        let old_source_dir = self.source_dir.clone();
        self.env = VmEnv::new();
        self.source_dir = module_source_dir.clone();

        for import in &artifact.imports {
            self.execute_import(&import.path, import.selected_names.as_deref())
                .await?;
        }

        let module_state: crate::value::ModuleState = {
            let mut init_env = self.env.clone();
            if let Some(init_chunk) = &artifact.init_chunk {
                let fresh_init_chunk = Chunk::from_cached(init_chunk);
                let saved_env = std::mem::replace(&mut self.env, init_env);
                let saved_frames = std::mem::take(&mut self.frames);
                let saved_handlers = std::mem::take(&mut self.exception_handlers);
                let saved_iterators = std::mem::take(&mut self.iterators);
                let saved_deadlines = std::mem::take(&mut self.deadlines);
                // STEP_STACK / PERSONA_STACK are thread-locals shared with
                // the calling frame. Emptying `self.frames` above means
                // any `prune_below_frame(0)` triggered while the init
                // chunk's bytecode runs — including the inevitable
                // frame-pop prune at end-of-chunk — would wipe active
                // steps owned by the *caller* (e.g., a `@step`-decorated
                // function whose body lazily imports a module). Snapshot
                // the persona/step context here and restore it after init
                // so module loading is invisible to the step-tracking
                // surface.
                let active_context = crate::step_runtime::take_active_context();
                let init_result = self.run_chunk(&fresh_init_chunk).await;
                crate::step_runtime::restore_active_context(active_context);
                init_env = std::mem::replace(&mut self.env, saved_env);
                self.frames = saved_frames;
                self.exception_handlers = saved_handlers;
                self.iterators = saved_iterators;
                self.deadlines = saved_deadlines;
                init_result?;
            }
            Arc::new(crate::value::VmMutex::new(init_env))
        };

        let module_env = self.env.clone();
        let registry: ModuleFunctionRegistry =
            Arc::new(crate::value::VmMutex::new(BTreeMap::new()));
        let mut functions: BTreeMap<String, Arc<VmClosure>> = BTreeMap::new();
        let mut public_names = artifact.public_names.clone();

        for (name, compiled) in &artifact.functions {
            let closure = Arc::new(VmClosure {
                func: Arc::new(CompiledFunction::from_cached(compiled)),
                env: module_env.clone(),
                source_dir: module_source_dir.clone(),
                module_functions: Some(Arc::downgrade(&registry)),
                module_state: Some(Arc::downgrade(&module_state)),
            });
            registry.lock().insert(name.clone(), Arc::clone(&closure));
            self.env
                .define(name, VmValue::Closure(Arc::clone(&closure)), false)?;
            module_state
                .lock()
                .define(name, VmValue::Closure(Arc::clone(&closure)), false)?;
            functions.insert(name.clone(), Arc::clone(&closure));
        }

        for import in artifact.imports.iter().filter(|import| import.is_pub) {
            let cache_key = self.cache_key_for_import(&import.path);
            let Some(loaded) = self.module_cache.get(&cache_key).cloned() else {
                return Err(VmError::Runtime(format!(
                    "Re-export error: imported module '{}' was not loaded",
                    import.path
                )));
            };
            let names_to_reexport: Vec<String> = match &import.selected_names {
                Some(names) => names.clone(),
                None => {
                    if loaded.public_names.is_empty() {
                        loaded.functions.keys().cloned().collect()
                    } else {
                        loaded.public_names.iter().cloned().collect()
                    }
                }
            };
            for name in names_to_reexport {
                let Some(closure) = loaded.functions.get(&name) else {
                    return Err(VmError::Runtime(format!(
                        "Re-export error: '{name}' is not exported by '{}'",
                        import.path
                    )));
                };
                if let Some(existing) = functions.get(&name) {
                    if !Arc::ptr_eq(existing, closure) {
                        return Err(VmError::Runtime(format!(
                            "Re-export collision: '{name}' is defined here and also \
                             re-exported from '{}'",
                            import.path
                        )));
                    }
                }
                functions.insert(name.clone(), Arc::clone(closure));
                public_names.insert(name);
            }
        }

        self.env = caller_env;
        self.source_dir = old_source_dir;

        Ok(LoadedModule {
            functions,
            public_names,
            _module_functions: registry,
            _module_state: module_state,
        })
    }

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
                .define(&name, VmValue::Closure(Arc::clone(closure)), false)?;
        }
        Ok(())
    }

    /// Execute an import, reading and running the file's declarations.
    pub(super) fn execute_import<'a>(
        &'a mut self,
        path: &'a str,
        selected_names: Option<&'a [String]>,
    ) -> Pin<Box<dyn Future<Output = Result<(), VmError>> + Send + 'a>> {
        Box::pin(async move {
            let _import_span = ScopeSpan::new(crate::tracing::SpanKind::Import, path.to_string());

            let stdlib_module = path
                .strip_prefix("std/")
                .or_else(|| (path == "observability").then_some("observability"));
            if let Some(module) = stdlib_module {
                if let Some(source) = crate::stdlib_modules::get_stdlib_source(module) {
                    let synthetic = PathBuf::from(format!("<stdlib>/{module}.harn"));
                    if self.imported_paths.contains(&synthetic) {
                        return Ok(());
                    }
                    let loaded = self
                        .load_stdlib_module_from_source(module, synthetic.clone(), source)
                        .await?;
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
            let file_path = resolve_module_import_path(&base, path);

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
                // Name the resolution base: relative imports resolve against the
                // importing file's dir (or CWD when unset), so an error that
                // prints only the joined path leaves the author guessing which
                // base was used.
                VmError::Runtime(format!(
                    "Import error: cannot read '{}' (resolved '{path}' relative to {}): {e}",
                    file_path.display(),
                    base.display()
                ))
            })?;
            Arc::make_mut(&mut self.source_cache).insert(canonical.clone(), source.clone());
            Arc::make_mut(&mut self.source_cache).insert(file_path.clone(), source.clone());

            // Disk cache first: hits skip parse + compile for the imported
            // module's whole function pool, not just the entry pipeline.
            let lookup = bytecode_cache::load_module(&file_path, &source);
            let artifact = if let Some(artifact) = lookup.artifact {
                artifact
            } else {
                let compiled = compile_module_artifact_from_source(&file_path, &source)?;
                if let Err(err) = bytecode_cache::store_module(&lookup.key, &compiled) {
                    if std::env::var_os("HARN_BYTECODE_CACHE_DEBUG").is_some() {
                        eprintln!(
                            "[harn] module cache write skipped for {}: {err}",
                            file_path.display()
                        );
                    }
                }
                compiled
            };

            let module_source_dir = file_path.parent().map(|p| p.to_path_buf());
            let loaded = self
                .instantiate_module(module_source_dir, &artifact)
                .await?;
            self.imported_paths.pop();
            Arc::make_mut(&mut self.module_cache).insert(canonical.clone(), loaded.clone());
            self.export_loaded_module(&canonical, &loaded, selected_names)?;

            Ok(())
        })
    }

    /// Return the path key that `execute_import` would use to cache the
    /// LoadedModule for this import string. Used by the re-export pass to
    /// look up the already-loaded source module after `execute_import`
    /// has populated [`Vm::module_cache`].
    fn cache_key_for_import(&self, path: &str) -> PathBuf {
        if let Some(module) = path
            .strip_prefix("std/")
            .or_else(|| (path == "observability").then_some("observability"))
        {
            return PathBuf::from(format!("<stdlib>/{module}.harn"));
        }
        let base = self
            .source_dir
            .clone()
            .unwrap_or_else(|| PathBuf::from("."));
        let file_path = resolve_module_import_path(&base, path);
        file_path.canonicalize().unwrap_or(file_path)
    }

    /// Load a module file and return the exported function closures that
    /// would be visible to a wildcard import.
    pub async fn load_module_exports(
        &mut self,
        path: &Path,
    ) -> Result<BTreeMap<String, Arc<VmClosure>>, VmError> {
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
            exports.insert(name, Arc::clone(closure));
        }

        Ok(exports)
    }

    /// Load synthetic source keyed by a synthetic module path and return
    /// the exported function closures that a wildcard import would expose.
    pub async fn load_module_exports_from_source(
        &mut self,
        source_key: impl Into<PathBuf>,
        source: &str,
    ) -> Result<BTreeMap<String, Arc<VmClosure>>, VmError> {
        let synthetic = source_key.into();
        let loaded = self
            .load_module_from_source(synthetic.clone(), source)
            .await?;
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
                    synthetic.display()
                )));
            };
            exports.insert(name, Arc::clone(closure));
        }

        Ok(exports)
    }

    /// Load a module by import path (`std/foo`, relative module path, or
    /// package import) and return the exported function closures that a
    /// wildcard import would expose.
    pub async fn load_module_exports_from_import(
        &mut self,
        import_path: &str,
    ) -> Result<BTreeMap<String, Arc<VmClosure>>, VmError> {
        self.execute_import(import_path, None).await?;

        if let Some(module) = import_path
            .strip_prefix("std/")
            .or_else(|| (import_path == "observability").then_some("observability"))
        {
            let synthetic = PathBuf::from(format!("<stdlib>/{module}.harn"));
            let loaded = self.module_cache.get(&synthetic).cloned().ok_or_else(|| {
                VmError::Runtime(format!(
                    "Import error: failed to cache loaded module '{}'",
                    synthetic.display()
                ))
            })?;
            let mut exports = BTreeMap::new();
            let export_names: Vec<String> = if loaded.public_names.is_empty() {
                loaded.functions.keys().cloned().collect()
            } else {
                loaded.public_names.iter().cloned().collect()
            };
            for name in export_names {
                let Some(closure) = loaded.functions.get(&name) else {
                    return Err(VmError::Runtime(format!(
                        "Import error: exported function '{name}' is missing from {}",
                        synthetic.display()
                    )));
                };
                exports.insert(name, Arc::clone(closure));
            }
            return Ok(exports);
        }

        let base = self
            .source_dir
            .clone()
            .unwrap_or_else(|| PathBuf::from("."));
        let file_path = resolve_module_import_path(&base, import_path);
        self.load_module_exports(&file_path).await
    }
}

#[cfg(test)]
mod tests {

    use std::sync::{Mutex, MutexGuard, OnceLock};

    use super::*;

    static CACHE_TEST_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

    fn cache_test_guard() -> MutexGuard<'static, ()> {
        CACHE_TEST_LOCK
            .get_or_init(|| Mutex::new(()))
            .lock()
            .unwrap()
    }

    fn cached_stdlib_module_ptr(module: &str) -> Option<usize> {
        let source = harn_stdlib::get_stdlib_source(module).expect("stdlib module source exists");
        stdlib_module_artifact_cache_ptr(module, source)
    }

    #[test]
    fn stdlib_artifact_cache_reuses_compilation_with_fresh_vm_state() {
        let _guard = cache_test_guard();
        reset_stdlib_module_artifact_cache();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime builds");

        let (first_exports, second_exports, first_state_weak, second_state_weak) = runtime
            .block_on(async {
                let mut first_vm = Vm::new();
                let first_exports = first_vm
                    .load_module_exports_from_import("std/agent/prompts")
                    .await
                    .expect("first stdlib import succeeds");
                let first_state = first_exports
                    .get("render_agent_prompt")
                    .expect("first export exists")
                    .module_state()
                    .expect("first module state stays live while VM owns module");
                let first_state_weak = Arc::downgrade(&first_state);
                let first_state_ptr = Arc::as_ptr(&first_state);

                let mut second_vm = Vm::new();
                let second_exports = second_vm
                    .load_module_exports_from_import("std/agent/prompts")
                    .await
                    .expect("second stdlib import succeeds");
                let second_state = second_exports
                    .get("render_agent_prompt")
                    .expect("second export exists")
                    .module_state()
                    .expect("second module state stays live while VM owns module");
                let second_state_weak = Arc::downgrade(&second_state);

                assert_ne!(first_state_ptr, Arc::as_ptr(&second_state));
                (
                    first_exports,
                    second_exports,
                    first_state_weak,
                    second_state_weak,
                )
            });
        let first_cached =
            cached_stdlib_module_ptr("agent/prompts").expect("first import cached stdlib artifact");
        assert_eq!(
            cached_stdlib_module_ptr("agent/prompts"),
            Some(first_cached)
        );

        let first = first_exports
            .get("render_agent_prompt")
            .expect("first export exists");
        let second = second_exports
            .get("render_agent_prompt")
            .expect("second export exists");

        assert!(!Arc::ptr_eq(first, second));
        assert!(!Arc::ptr_eq(&first.func, &second.func));
        assert!(!Arc::ptr_eq(&first.func.chunk, &second.func.chunk));
        assert!(first.module_state().is_none());
        assert!(second.module_state().is_none());
        assert!(first_state_weak.upgrade().is_none());
        assert!(second_state_weak.upgrade().is_none());
    }

    #[test]
    fn stdlib_artifact_cache_is_process_wide_across_threads() {
        let _guard = cache_test_guard();
        reset_stdlib_module_artifact_cache();

        let handle = std::thread::spawn(|| {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("runtime builds");
            runtime.block_on(async {
                let mut vm = Vm::new();
                vm.load_module_exports_from_import("std/agent/prompts")
                    .await
                    .expect("thread stdlib import succeeds");
            });
        });
        handle.join().expect("thread joins");
        let thread_cached = cached_stdlib_module_ptr("agent/prompts")
            .expect("thread import cached stdlib artifact");

        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime builds");
        runtime.block_on(async {
            let mut vm = Vm::new();
            vm.load_module_exports_from_import("std/agent/prompts")
                .await
                .expect("main-thread stdlib import succeeds");
        });
        assert_eq!(
            cached_stdlib_module_ptr("agent/prompts"),
            Some(thread_cached)
        );
    }

    #[test]
    fn module_closures_release_state_after_vm_drop() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime builds");

        let (closure_weak, registry_weak, state_weak) = runtime.block_on(async {
            let mut vm = Vm::new();
            let loaded = vm
                .load_module_from_source(
                    PathBuf::from("<test>/module_cycle.harn"),
                    r#"
var payload = "x" * 1024

pub fn touch() {
  return len(payload)
}
"#,
                )
                .await
                .expect("module loads");
            let closure = Arc::clone(loaded.functions.get("touch").expect("touch export exists"));
            let closure_weak = Arc::downgrade(&closure);
            let registry_weak = Arc::downgrade(&loaded._module_functions);
            let state_weak = Arc::downgrade(&loaded._module_state);

            drop(closure);
            drop(loaded);
            drop(vm);

            (closure_weak, registry_weak, state_weak)
        });

        assert!(
            closure_weak.upgrade().is_none(),
            "module closure should drop with its VM"
        );
        assert!(
            registry_weak.upgrade().is_none(),
            "module function registry should drop with its VM"
        );
        assert!(
            state_weak.upgrade().is_none(),
            "module state should drop with its VM"
        );
    }
}
