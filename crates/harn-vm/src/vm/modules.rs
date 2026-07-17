use std::collections::{BTreeMap, HashSet};
use std::future::Future;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::{Arc, Mutex, OnceLock};

use crate::bytecode_cache;
use crate::module_artifact::compile_module_artifact_from_source;
use crate::prepared_module::PreparedModuleArtifact;
use crate::value::{ModuleFunctionRegistry, VmClosure, VmEnv, VmError, VmValue};

use super::{ScopeSpan, Vm};

static STDLIB_MODULE_ARTIFACT_CACHE: OnceLock<
    Mutex<BTreeMap<String, Arc<PreparedModuleArtifact>>>,
> = OnceLock::new();

fn stdlib_module_artifact_cache() -> &'static Mutex<BTreeMap<String, Arc<PreparedModuleArtifact>>> {
    STDLIB_MODULE_ARTIFACT_CACHE.get_or_init(|| Mutex::new(BTreeMap::new()))
}

fn verified_package_source(bytes: Vec<u8>, path: &Path) -> Result<String, VmError> {
    String::from_utf8(bytes).map_err(|error| {
        VmError::Runtime(format!(
            "installed package source {} is not valid UTF-8: {error}",
            path.display()
        ))
    })
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

pub(crate) struct LoadedModule {
    pub(crate) functions: BTreeMap<String, Arc<VmClosure>>,
    pub(crate) public_names: HashSet<String>,
    /// Evaluated values of exported `pub const` / `pub let` bindings, read out
    /// of the instantiated module env after the init chunk ran. Importers bind
    /// these by value (like every other cross-module value). Disjoint from
    /// `functions` and `public_type_names`.
    pub(crate) public_values: BTreeMap<String, VmValue>,
    /// Names of `pub type` aliases (and re-exported ones). Erased at runtime:
    /// selective imports may name them, but they bind no value of their own.
    pub(crate) public_type_names: HashSet<String>,
    /// Decoded JSON-Schema dict for each `pub type` alias that lowers to a
    /// schema. Importers bind the alias name to this value so
    /// expression-position uses (`output_schema: ImportedAlias`) work.
    pub(crate) public_type_schemas: BTreeMap<String, VmValue>,
    pub(crate) _module_functions: crate::value::ModuleFunctionRegistry,
    pub(crate) _module_state: crate::value::ModuleState,
}

/// Runtime module cache shared by child VMs within one execution tree.
///
/// The map stays copy-on-write so a child can add modules without mutating its
/// parent. Cache entries are never replaced after instantiation, so cache hits
/// and map copies share their export maps plus their existing shared
/// registries/state through a cheap outer [`Arc`] instead of cloning the whole
/// module.
pub(crate) type ModuleCache = Arc<BTreeMap<PathBuf, Arc<LoadedModule>>>;

/// An import whose target module was still mid-load (an import cycle) when the
/// importing module reached it. The target's function closures don't exist yet
/// at that point, so the binding can't happen inline. We record it here and
/// resolve it once both modules are fully loaded — see
/// [`Vm::flush_deferred_cyclic_imports`].
#[derive(Clone, Debug)]
pub(crate) struct DeferredCyclicImport {
    /// Canonical path of the module that issued the import.
    pub(crate) importer: PathBuf,
    /// Canonical path of the cyclically-imported target module.
    pub(crate) target: PathBuf,
    /// Selectively-imported names, or `None` for a wildcard/side-effect import.
    pub(crate) selected_names: Option<Vec<String>>,
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
    recorder: Option<&super::ModulePhaseRecorder>,
) -> Result<Arc<PreparedModuleArtifact>, VmError> {
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
    let lookup = {
        let _load_span = recorder.map(super::ModulePhaseRecorder::load_span);
        bytecode_cache::load_module(synthetic, source)
    };
    let artifact = if let Some(artifact) = lookup.artifact {
        artifact
    } else {
        let mut compile_span = recorder.map(super::ModulePhaseRecorder::compile_span);
        let compiled = compile_module_artifact_from_source(synthetic, source)?;
        if let Some(span) = &mut compile_span {
            span.mark_compile_succeeded();
        }
        drop(compile_span);
        if let Err(err) = bytecode_cache::store_module(&lookup.key, &compiled) {
            if std::env::var_os("HARN_BYTECODE_CACHE_DEBUG").is_some() {
                eprintln!("[harn] stdlib module cache write skipped for {module}: {err}");
            }
        }
        compiled
    };

    let compiled = {
        let _load_span = recorder.map(super::ModulePhaseRecorder::load_span);
        Arc::new(PreparedModuleArtifact::from_cached(artifact))
    };
    let mut cache = stdlib_module_artifact_cache().lock().unwrap();
    if let Some(cached) = cache.get(&key) {
        return Ok(Arc::clone(cached));
    }
    cache.insert(key, Arc::clone(&compiled));
    Ok(compiled)
}

impl Vm {
    fn resolve_module_import_path(&self, base: &Path, path: &str) -> Result<PathBuf, VmError> {
        if let Some(guard) = &self.package_execution_guard {
            let synthetic_current_file = base.join("__harn_import_base__.harn");
            if let Some(resolved) =
                harn_modules::resolve_import_path_with_guard(&synthetic_current_file, path, guard)
                    .map_err(|error| {
                    VmError::Runtime(format!("installed package import rejected: {error}"))
                })?
            {
                return Ok(resolved);
            }
            let mut file_path = base.join(path);
            if !file_path.exists() && file_path.extension().is_none() {
                file_path.set_extension("harn");
            }
            return Ok(file_path);
        }
        Ok(resolve_module_import_path(base, path))
    }

    /// Resolve a callable against this VM. Lazy callables initialize once per
    /// VM execution tree, then retain that module scope for later child VMs in
    /// the same tree. Fresh VM roots remain isolated.
    pub async fn resolve_callable(
        &mut self,
        callable: &crate::value::VmCallable,
    ) -> Result<Arc<crate::value::VmClosure>, VmError> {
        self.ensure_execution_available()?;
        match callable {
            crate::value::VmCallable::Eager(closure) => Ok(Arc::clone(closure)),
            crate::value::VmCallable::Lazy(lazy) => {
                let (cache_key, module_path) = self.lazy_callable_module_path(lazy);
                let resolution = {
                    let mut modules = self.lazy_callable_modules.lock();
                    Arc::clone(
                        modules
                            .entry(cache_key)
                            .or_insert_with(|| Arc::new(tokio::sync::OnceCell::new())),
                    )
                };
                let resolved = resolution
                    .get_or_try_init(|| async {
                        let exports = self.load_module_exports(&module_path).await?;
                        let exports = exports
                            .into_iter()
                            .map(|(name, closure)| (name, closure.retained_for_host_registry()))
                            .collect();
                        // Pin the complete module graph loaded above so that a
                        // handler's transitively imported callees keep their
                        // home-module registries/state alive for later child
                        // VMs that hit this cache without re-importing.
                        Ok::<_, VmError>(Arc::new(crate::vm::state::ResolvedLazyCallable {
                            exports,
                            retained_module_graph: Arc::clone(&self.module_cache),
                        }))
                    })
                    .await?;
                resolved
                    .exports
                    .get(&lazy.function_name)
                    .cloned()
                    .ok_or_else(|| {
                        VmError::Runtime(format!(
                            "function '{}' is not exported by module '{}'",
                            lazy.function_name,
                            lazy.module_path.display()
                        ))
                    })
            }
            crate::value::VmCallable::Pipeline(_) => Err(VmError::TypeError(
                "pipeline callable requires execute_callable".to_string(),
            )),
        }
    }

    pub async fn execute_callable(
        &mut self,
        callable: &crate::value::VmCallable,
        args: &[crate::value::VmValue],
    ) -> Result<crate::value::VmValue, VmError> {
        let crate::value::VmCallable::Pipeline(pipeline) = callable else {
            let closure = self.resolve_callable(callable).await?;
            return self.call_closure_pub(&closure, args).await;
        };

        let (_, module_path) = self.lazy_module_path(&pipeline.module_path);
        let next_guard = pipeline
            .package_execution_guard_handle()
            .or_else(|| self.package_execution_guard.clone());
        let source = if let Some(guard) = &next_guard {
            let bytes = guard.verify_entry_source(&module_path).map_err(|error| {
                VmError::Runtime(format!("installed package execution rejected: {error}"))
            })?;
            verified_package_source(bytes, &module_path)?
        } else {
            std::fs::read_to_string(&module_path).map_err(|error| {
                VmError::Runtime(format!("failed to read {}: {error}", module_path.display()))
            })?
        };
        let program = harn_parser::check_source_strict(&source)
            .map_err(|error| VmError::Runtime(error.to_string()))?;
        let params = program
            .iter()
            .find_map(|node| {
                let (_, inner) = harn_parser::peel_attributes(node);
                match &inner.node {
                    harn_parser::Node::Pipeline {
                        name,
                        params,
                        is_pub: true,
                        ..
                    } if name == &pipeline.pipeline_name => Some(params.clone()),
                    _ => None,
                }
            })
            .ok_or_else(|| {
                VmError::Runtime(format!(
                    "pipeline '{}' is not exported by module '{}'",
                    pipeline.pipeline_name,
                    module_path.display()
                ))
            })?;
        if params.len() != args.len() {
            return Err(VmError::Runtime(format!(
                "pipeline '{}' expects {} argument(s), got {}",
                pipeline.pipeline_name,
                params.len(),
                args.len()
            )));
        }
        let chunk = crate::Compiler::new()
            .compile_named_with_param_globals(&program, &pipeline.pipeline_name)
            .map_err(|error| VmError::Runtime(error.to_string()))?;
        let previous_source_dir = self.source_dir.clone();
        if let Some(parent) = module_path.parent() {
            self.set_source_dir(parent);
        }
        for (param, value) in params.iter().zip(args) {
            self.set_global(param, value.clone());
        }
        let previous_package_execution_guard =
            std::mem::replace(&mut self.package_execution_guard, next_guard);
        let result = self.execute(&chunk).await;
        self.package_execution_guard = previous_package_execution_guard;
        self.source_dir = previous_source_dir;
        crate::stdlib::set_thread_source_dir_option(self.source_dir.as_deref());
        result
    }

    fn lazy_callable_module_path(&self, lazy: &crate::value::LazyVmCallable) -> (PathBuf, PathBuf) {
        self.lazy_module_path(&lazy.module_path)
    }

    fn lazy_module_path(&self, path: &std::path::Path) -> (PathBuf, PathBuf) {
        let mut module_path = if path.is_absolute() {
            path.to_path_buf()
        } else {
            self.source_dir
                .clone()
                .unwrap_or_else(|| PathBuf::from("."))
                .join(path)
        };
        if !module_path.exists() && module_path.extension().is_none() {
            module_path.set_extension("harn");
        }
        let cache_key = module_path
            .canonicalize()
            .unwrap_or_else(|_| module_path.clone());
        (cache_key, module_path)
    }

    async fn load_module_from_source(
        &mut self,
        synthetic: PathBuf,
        source: &str,
    ) -> Result<Arc<LoadedModule>, VmError> {
        if let Some(loaded) = self.module_cache.get(&synthetic).cloned() {
            return Ok(loaded);
        }
        Arc::make_mut(&mut self.source_cache).insert(synthetic.clone(), source.to_string());

        let mut compile_span = self.module_compile_span();
        let compiled = compile_module_artifact_from_source(&synthetic, source)?;
        if let Some(span) = &mut compile_span {
            span.mark_compile_succeeded();
        }
        drop(compile_span);
        let artifact = {
            let _load_span = self.module_load_span();
            PreparedModuleArtifact::from_cached(compiled)
        };

        self.imported_paths.push(synthetic.clone());
        let loaded = Arc::new(self.instantiate_module(None, &artifact).await?);
        self.imported_paths.pop();
        {
            let _load_span = self.module_load_span();
            Arc::make_mut(&mut self.module_cache).insert(synthetic, Arc::clone(&loaded));
        }
        self.record_module_loaded();
        Ok(loaded)
    }

    async fn load_stdlib_module_from_source(
        &mut self,
        module: &str,
        synthetic: PathBuf,
        source: &'static str,
    ) -> Result<Arc<LoadedModule>, VmError> {
        if let Some(loaded) = self.module_cache.get(&synthetic).cloned() {
            return Ok(loaded);
        }
        Arc::make_mut(&mut self.source_cache).insert(synthetic.clone(), source.to_string());

        let artifact = stdlib_module_artifact(
            module,
            &synthetic,
            source,
            self.module_phase_recorder.as_ref(),
        )?;
        self.imported_paths.push(synthetic.clone());
        let loaded = Arc::new(self.instantiate_stdlib_module(artifact.as_ref()).await?);
        self.imported_paths.pop();
        {
            let _load_span = self.module_load_span();
            Arc::make_mut(&mut self.module_cache).insert(synthetic, Arc::clone(&loaded));
        }
        self.record_module_loaded();
        Ok(loaded)
    }

    async fn instantiate_stdlib_module(
        &mut self,
        artifact: &PreparedModuleArtifact,
    ) -> Result<LoadedModule, VmError> {
        self.instantiate_module(None, artifact).await
    }

    /// Instantiate a previously-hydrated [`PreparedModuleArtifact`] into a
    /// [`LoadedModule`]. Re-runs nested imports, replays the init chunk
    /// into a fresh module env, mints a [`VmClosure`] for each compiled
    /// function (stamped with `module_source_dir` so imports from inside
    /// those functions resolve against the originating file), and
    /// applies the re-export pass. Used by both stdlib and user-import
    /// code paths.
    async fn instantiate_module(
        &mut self,
        module_source_dir: Option<PathBuf>,
        artifact: &PreparedModuleArtifact,
    ) -> Result<LoadedModule, VmError> {
        let caller_env = self.env.clone();
        let old_source_dir = self.source_dir.clone();
        self.env = VmEnv::new();
        self.source_dir = module_source_dir.clone();

        for import in &artifact.imports {
            self.execute_import(&import.path, import.selected_names.as_deref())
                .await?;
        }

        // Nested modules own their own load spans. Start this module's span
        // only after those imports finish so aggregate load time is additive.
        let _load_span = self.module_load_span();

        let module_state: crate::value::ModuleState = {
            let mut init_env = self.env.clone();
            if let Some(init_chunk) = &artifact.init_chunk {
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
                let init_result = self.run_chunk(Arc::clone(init_chunk)).await;
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
        // `pub const` / `pub let`: the init chunk already ran into `module_state`,
        // so the bound values are live there. Read each exported value name out
        // and publish it so importers can bind it. Add the names to
        // `public_names` too, so the selective/wildcard export machinery treats
        // them as part of the public surface (validation, re-export lists).
        let mut public_values: BTreeMap<String, VmValue> = BTreeMap::new();
        {
            let state = module_state.lock();
            for name in &artifact.public_value_names {
                if let Some(value) = state.get(name) {
                    public_values.insert(name.clone(), value);
                    public_names.insert(name.clone());
                }
            }
        }
        let mut public_type_names = artifact.public_type_names.clone();
        let mut public_type_schemas: BTreeMap<String, VmValue> = artifact
            .public_type_schemas
            .iter()
            .filter_map(|(name, json)| {
                let parsed = serde_json::from_str::<serde_json::Value>(json).ok()?;
                Some((name.clone(), crate::schema::json_to_vm_value(&parsed)))
            })
            .collect();

        for (name, compiled) in &artifact.functions {
            let closure = Arc::new(VmClosure {
                func: Arc::clone(compiled),
                env: module_env.clone(),
                source_dir: module_source_dir.clone(),
                module_functions: Some(Arc::downgrade(&registry)),
                module_state: Some(Arc::downgrade(&module_state)),
                retained_module_scope: None,
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
            let cache_key = self.cache_key_for_import(&import.path)?;
            let Some(loaded) = self.module_cache.get(&cache_key).cloned() else {
                // A plain `import`/`import {...}` across a cycle is bound late
                // by `flush_deferred_cyclic_imports`, but a `pub import`
                // re-export has to publish the names into *this* module's
                // public surface right now — and the target is still mid-load,
                // so its surface does not exist yet. Name the cycle explicitly
                // instead of the misleading "was not loaded".
                if self.imported_paths.contains(&cache_key) {
                    return Err(VmError::Runtime(format!(
                        "Re-export error: cannot `pub import` from '{}' because it forms an \
                         import cycle with this module (its public surface is still being \
                         built). Use a plain `import` here, or re-export from a module that is \
                         not part of the cycle.",
                        import.path
                    )));
                }
                return Err(VmError::Runtime(format!(
                    "Re-export error: imported module '{}' was not loaded",
                    import.path
                )));
            };
            let names_to_reexport: Vec<String> = match &import.selected_names {
                Some(names) => names.clone(),
                // A wildcard `pub import` re-exports exactly the target's `pub`
                // surface (functions and erased `pub type` aliases). A module
                // with no `pub` declarations exports nothing.
                None => loaded
                    .public_names
                    .iter()
                    .chain(loaded.public_type_names.iter())
                    .cloned()
                    .collect(),
            };
            for name in names_to_reexport {
                let Some(closure) = loaded.functions.get(&name) else {
                    // `pub const` / `pub let` values carry no closure: re-export
                    // the value directly.
                    if let Some(value) = loaded.public_values.get(&name) {
                        public_values.insert(name.clone(), value.clone());
                        public_names.insert(name);
                        continue;
                    }
                    // `pub type` aliases are erased at runtime: re-export the
                    // name (and its schema lowering, when present) for
                    // importers, with no closure to bind.
                    if loaded.public_type_names.contains(&name) {
                        if let Some(schema) = loaded.public_type_schemas.get(&name) {
                            public_type_schemas.insert(name.clone(), schema.clone());
                        }
                        public_type_names.insert(name);
                        continue;
                    }
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
            public_values,
            public_type_names,
            public_type_schemas,
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
        let module_name = module_path.display().to_string();
        let export_names: Vec<String> = if let Some(names) = selected_names {
            // Selective imports may only name symbols the module marks `pub`.
            // A module with no `pub` functions exports nothing — matching every
            // strict-visibility language (TypeScript, Rust, Go) and removing
            // the old footgun where adding the first `pub` silently turned every
            // other (previously importable) function private to callers.
            for name in names {
                if !loaded.public_names.contains(name) && !loaded.public_type_names.contains(name) {
                    let hint = if loaded.functions.contains_key(name) {
                        " — it is defined there but not `pub`; mark it `pub` to export it"
                    } else {
                        ""
                    };
                    return Err(VmError::Runtime(format!(
                        "Import error: '{name}' is not exported by {module_name}{hint}"
                    )));
                }
            }
            names.to_vec()
        } else {
            // Wildcard import brings in exactly the module's `pub` surface,
            // including erased `pub type` aliases.
            loaded
                .public_names
                .iter()
                .chain(loaded.public_type_names.iter())
                .cloned()
                .collect()
        };

        for name in export_names {
            // `pub type` aliases are erased at runtime: the import is valid
            // (the type checker consumed it). When the alias lowers to a JSON
            // schema, bind the name to that dict so expression-position uses
            // (`output_schema: ImportedAlias`, `schema_is(x, ImportedAlias)`)
            // behave like a locally declared alias; otherwise bind nothing.
            if loaded.public_type_names.contains(&name) && !loaded.functions.contains_key(&name) {
                if let Some(schema) = loaded.public_type_schemas.get(&name) {
                    self.env.define(&name, schema.clone(), false)?;
                }
                continue;
            }
            // `pub const` / `pub let` values: bind by value.
            if let Some(value) = loaded.public_values.get(&name) {
                if self.env.get(&name).is_some() {
                    return Err(VmError::Runtime(format!(
                        "Import collision: '{name}' is already defined when importing \
                         {module_name}. Use selective imports to disambiguate: \
                         import {{ {name} }} from \"...\""
                    )));
                }
                self.env.define(&name, value.clone(), false)?;
                continue;
            }
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
                    if let Some(loaded) = self.module_cache.get(&synthetic).cloned() {
                        return self.export_loaded_module(&synthetic, &loaded, selected_names);
                    }
                    let loaded = self
                        .load_stdlib_module_from_source(module, synthetic.clone(), source)
                        .await?;
                    {
                        let _load_span = self.module_load_span();
                        self.export_loaded_module(&synthetic, &loaded, selected_names)?;
                    }
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
            let file_path = self.resolve_module_import_path(&base, path)?;
            let verified_source = if let Some(guard) = &self.package_execution_guard {
                let bytes = guard.verify_entry_source(&file_path).map_err(|error| {
                    VmError::Runtime(format!("installed package import rejected: {error}"))
                })?;
                Some(verified_package_source(bytes, &file_path)?)
            } else {
                None
            };

            let canonical = file_path
                .canonicalize()
                .unwrap_or_else(|_| file_path.clone());
            if self.imported_paths.contains(&canonical) {
                // Import cycle: `canonical` is still mid-load (it sits on the
                // import stack), so its function closures don't exist yet and
                // we cannot bind the requested names inline. Record the import
                // and resolve it once both modules finish loading — otherwise
                // whichever module happens to close the cycle silently loses
                // these bindings and fails with `Undefined builtin` at call
                // time, in a load-order-dependent way.
                if let Some(importer) = self.imported_paths.last().cloned() {
                    if importer != canonical {
                        self.deferred_cyclic_imports.push(DeferredCyclicImport {
                            importer,
                            target: canonical.clone(),
                            selected_names: selected_names.map(<[String]>::to_vec),
                        });
                    }
                }
                return Ok(());
            }
            if let Some(loaded) = self.module_cache.get(&canonical).cloned() {
                if let Some(source) = &verified_source {
                    let cached_source = self.source_cache.get(&canonical);
                    if cached_source != Some(source) {
                        return Err(VmError::Runtime(format!(
                            "installed package import rejected: cached module {} was not compiled from the verified package bytes",
                            canonical.display()
                        )));
                    }
                }
                return self.export_loaded_module(&canonical, &loaded, selected_names);
            }
            self.imported_paths.push(canonical.clone());

            let source = {
                let _load_span = self.module_load_span();
                match verified_source {
                    Some(source) => source,
                    None => std::fs::read_to_string(&file_path).map_err(|e| {
                        // Name the resolution base: relative imports resolve against the
                        // importing file's dir (or CWD when unset), so an error that
                        // prints only the joined path leaves the author guessing which
                        // base was used.
                        VmError::Runtime(format!(
                            "Import error: cannot read '{}' (resolved '{path}' relative to {}): {e}",
                            file_path.display(),
                            base.display()
                        ))
                    })?,
                }
            };
            Arc::make_mut(&mut self.source_cache).insert(canonical.clone(), source.clone());
            Arc::make_mut(&mut self.source_cache).insert(file_path.clone(), source.clone());

            let prepared = {
                let _load_span = self.module_load_span();
                if bytecode_cache::cache_enabled() {
                    self.prepared_module_cache.get(&canonical, &source)
                } else {
                    None
                }
            };
            let artifact = if let Some(prepared) = prepared {
                prepared
            } else {
                // Disk cache hits skip parse + compile. The scoped prepared
                // cache additionally skips deserialization and chunk hydration
                // on later fresh VMs without sharing any runtime module state.
                let lookup = {
                    let _load_span = self.module_load_span();
                    bytecode_cache::load_module(&file_path, &source)
                };
                let cached = if let Some(artifact) = lookup.artifact {
                    artifact
                } else {
                    let mut compile_span = self.module_compile_span();
                    let compiled = compile_module_artifact_from_source(&file_path, &source)?;
                    if let Some(span) = &mut compile_span {
                        span.mark_compile_succeeded();
                    }
                    drop(compile_span);
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
                let mut prepared = {
                    let _load_span = self.module_load_span();
                    Arc::new(PreparedModuleArtifact::from_cached(cached))
                };
                if bytecode_cache::cache_enabled() {
                    prepared =
                        self.prepared_module_cache
                            .insert(canonical.clone(), &source, prepared);
                }
                prepared
            };

            let module_source_dir = file_path.parent().map(|p| p.to_path_buf());
            let loaded = Arc::new(
                self.instantiate_module(module_source_dir, artifact.as_ref())
                    .await?,
            );
            self.imported_paths.pop();
            {
                let _load_span = self.module_load_span();
                Arc::make_mut(&mut self.module_cache)
                    .insert(canonical.clone(), Arc::clone(&loaded));
            }
            self.record_module_loaded();
            {
                let _load_span = self.module_load_span();
                self.export_loaded_module(&canonical, &loaded, selected_names)?;
            }

            // Once the import stack fully unwinds, every module reachable from
            // this top-level import is cached, so any deferred cyclic imports
            // can now bind against fully-loaded modules.
            if self.imported_paths.is_empty() {
                let _load_span = self.module_load_span();
                self.flush_deferred_cyclic_imports()?;
            }

            Ok(())
        })
    }

    /// Bind imports that were deferred because their target module was still
    /// mid-load (an import cycle). By the time the import stack has unwound,
    /// both the importing and target modules are fully instantiated and cached,
    /// so we can resolve the requested names against the target and define them
    /// into the importer's shared, mutable `module_state`. That env is the one
    /// every closure from the importing module consults (after its local env)
    /// at call time, so the late binding becomes visible without needing to
    /// rewrite the closures' captured lexical snapshots.
    fn flush_deferred_cyclic_imports(&mut self) -> Result<(), VmError> {
        if self.deferred_cyclic_imports.is_empty() {
            return Ok(());
        }
        let deferred = std::mem::take(&mut self.deferred_cyclic_imports);
        let mut still_pending = Vec::new();
        for import in deferred {
            let (Some(importer), Some(target)) = (
                self.module_cache.get(&import.importer).cloned(),
                self.module_cache.get(&import.target).cloned(),
            ) else {
                // One endpoint is not cached yet (a lazy import inside a
                // function body can defer before the other side loads). Keep
                // it for a later flush.
                still_pending.push(import);
                continue;
            };

            let export_names: Vec<String> = match &import.selected_names {
                Some(names) => names.clone(),
                None if !target.public_names.is_empty() => {
                    target.public_names.iter().cloned().collect()
                }
                None => target.functions.keys().cloned().collect(),
            };

            let mut module_state = importer._module_state.lock();
            for name in export_names {
                // A real local declaration (or an already-bound non-cyclic
                // import) wins over the cyclic re-binding.
                if module_state.get(&name).is_some() {
                    continue;
                }
                if let Some(closure) = target.functions.get(&name) {
                    module_state.define(&name, VmValue::Closure(Arc::clone(closure)), false)?;
                } else if let Some(value) = target.public_values.get(&name) {
                    // `pub const` / `pub let` imported across a cycle.
                    module_state.define(&name, value.clone(), false)?;
                } else {
                    return Err(VmError::Runtime(format!(
                        "Import error: '{name}' is not defined in {}",
                        import.target.display()
                    )));
                }
            }
        }
        self.deferred_cyclic_imports = still_pending;
        Ok(())
    }

    /// Return the path key that `execute_import` would use to cache the
    /// LoadedModule for this import string. Used by the re-export pass to
    /// look up the already-loaded source module after `execute_import`
    /// has populated [`Vm::module_cache`].
    fn cache_key_for_import(&self, path: &str) -> Result<PathBuf, VmError> {
        if let Some(module) = path
            .strip_prefix("std/")
            .or_else(|| (path == "observability").then_some("observability"))
        {
            return Ok(PathBuf::from(format!("<stdlib>/{module}.harn")));
        }
        let base = self
            .source_dir
            .clone()
            .unwrap_or_else(|| PathBuf::from("."));
        let file_path = self.resolve_module_import_path(&base, path)?;
        Ok(file_path.canonicalize().unwrap_or(file_path))
    }

    /// Load a module file and return the exported function closures that
    /// would be visible to a wildcard import.
    pub async fn load_module_exports(
        &mut self,
        path: &Path,
    ) -> Result<BTreeMap<String, Arc<VmClosure>>, VmError> {
        self.ensure_execution_available()?;
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
        self.ensure_execution_available()?;
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
        self.ensure_execution_available()?;
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
        let file_path = self.resolve_module_import_path(&base, import_path)?;
        self.load_module_exports(&file_path).await
    }
}

#[cfg(test)]
#[path = "modules_tests.rs"]
mod tests;
