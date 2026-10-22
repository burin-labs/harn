use super::*;

/// Import bindings that settle after the complete module graph is available.
#[derive(Clone, Debug)]
pub(crate) struct DeferredCyclicImport {
    pub(crate) importer: PathBuf,
    pub(crate) target: PathBuf,
    pub(crate) selected_names: Option<Vec<String>>,
    pub(crate) namespace: Option<DeferredNamespaceImport>,
}

#[derive(Clone, Debug)]
pub(crate) struct DeferredNamespaceImport {
    pub(crate) alias: String,
    pub(crate) members: Option<Vec<String>>,
    pub(crate) binding: NamespaceImportBinding,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum NamespaceImportBinding {
    Deferred,
    Projected,
}

impl Vm {
    /// Call a module export while preserving the caller's lexical bindings.
    /// Module initialization commits through the same transaction as imports.
    pub async fn call_module_export(
        &mut self,
        import_path: &str,
        export_name: &str,
        label: &str,
        args: &[VmValue],
    ) -> Result<VmValue, VmError> {
        let mut transaction = self.module_load_transaction();
        transaction.env = VmEnv::new();
        transaction.imported_paths.clear();
        let exports = transaction
            .load_module_exports_from_import(import_path)
            .await?;
        let saved_env = std::mem::take(&mut self.env);
        let saved_imported_paths = std::mem::take(&mut self.imported_paths);
        self.commit_module_load_transaction(&mut transaction);
        self.env = saved_env;
        self.imported_paths = saved_imported_paths;
        let closure = exports.get(export_name).ok_or_else(|| {
            VmError::Runtime(format!(
                "{label}: stdlib module {import_path} did not export `{export_name}`"
            ))
        })?;
        self.call_closure_pub(closure, args).await
    }

    /// Create an isolated VM for one module-graph transaction.
    pub(super) fn module_load_transaction(&self) -> Vm {
        let mut transaction = self.child_vm_inline();
        transaction.imported_paths = self.imported_paths.clone();
        transaction.deferred_cyclic_imports = self.deferred_cyclic_imports.clone();
        transaction.task_counter = self.task_counter;
        transaction.runtime_context_counter = self.runtime_context_counter;
        transaction.interrupt_handlers = self.interrupt_handlers.clone();
        transaction.next_interrupt_handle = self.next_interrupt_handle;
        transaction.staged_module_load_count = Some(0);
        transaction
    }

    /// Commit the portable effects of a successful module-graph transaction.
    pub(super) fn commit_module_load_transaction(&mut self, transaction: &mut Vm) {
        self.env = transaction.env.clone();
        self.module_cache = Arc::clone(&transaction.module_cache);
        self.source_cache = Arc::clone(&transaction.source_cache);
        self.imported_paths = transaction.imported_paths.clone();
        self.deferred_cyclic_imports = transaction.deferred_cyclic_imports.clone();
        self.output.push_str(&transaction.output);
        self.task_counter = transaction.task_counter;
        self.runtime_context_counter = transaction.runtime_context_counter;
        self.interrupt_handlers = transaction.interrupt_handlers.clone();
        self.next_interrupt_handle = transaction.next_interrupt_handle;
        let loaded_count = transaction.staged_module_load_count.take().unwrap_or(0);
        for _ in 0..loaded_count {
            self.record_module_loaded();
        }
        for (task_id, task) in std::mem::take(&mut transaction.spawned_tasks) {
            let previous = self.spawned_tasks.insert(task_id.clone(), task);
            debug_assert!(previous.is_none(), "module task id collision: {task_id}");
        }
    }

    /// Resolve all deferred cycle bindings as one atomic state update.
    pub(super) async fn flush_deferred_cyclic_imports(&mut self) -> Result<(), VmError> {
        if self.deferred_cyclic_imports.is_empty() {
            return Ok(());
        }
        let deferred = std::mem::take(&mut self.deferred_cyclic_imports);
        let mut still_pending = Vec::new();
        let mut imports = Vec::new();
        let mut staged_states: BTreeMap<PathBuf, VmEnv> = BTreeMap::new();
        for import in deferred {
            let (Some(importer), Some(target)) = (
                self.module_cache.get(&import.importer).cloned(),
                self.module_cache.get(&import.target).cloned(),
            ) else {
                still_pending.push(import);
                continue;
            };

            staged_states
                .entry(import.importer.clone())
                .or_insert_with(|| importer._module_state.lock().clone());
            staged_states
                .entry(import.target.clone())
                .or_insert_with(|| target._module_state.lock().clone());
            imports.push(import);
        }

        // Each pass only adds bindings. Opaque type-only aliases add none, so
        // they reach the same fixed point without becoming permissive schemas.
        loop {
            let before = self.initialized_schema_count(&staged_states);
            let mut bound_import = false;
            for import in imports.iter().filter(|import| import.namespace.is_none()) {
                let target = self.module_cache.get(&import.target).unwrap();
                let names = module_import_names(
                    &import.target.display().to_string(),
                    target,
                    import.selected_names.as_deref(),
                    ImportNameUse::Binding,
                    harn_modules::sibling_module_access(&import.importer, &import.target),
                )?;
                for name in names {
                    if staged_states[&import.importer].get(&name).is_some() {
                        continue;
                    }
                    let schema = target
                        .public_type_names
                        .contains(&name)
                        .then(|| staged_states[&import.target].get(&name))
                        .flatten();
                    let value = target
                        .functions
                        .get(&name)
                        .map(|closure| VmValue::Closure(Arc::clone(closure)))
                        .or_else(|| target.public_values.get(&name).cloned())
                        .or(schema);
                    if let Some(value) = value {
                        staged_states
                            .get_mut(&import.importer)
                            .unwrap()
                            .define(&name, value, false)?;
                        bound_import = true;
                    } else if target
                        .public_exports
                        .get(&name)
                        .or_else(|| target.sibling_exports.get(&name))
                        .is_none_or(|kind| kind.has_runtime_value())
                    {
                        return Err(VmError::Runtime(format!(
                            "Import error: '{name}' is not defined in {}",
                            import.target.display()
                        )));
                    }
                }
            }
            let paths: Vec<_> = staged_states.keys().cloned().collect();
            for path in paths {
                let loaded = Arc::clone(&self.module_cache[&path]);
                let env = staged_states.remove(&path).unwrap();
                let saved_source =
                    std::mem::replace(&mut self.source_dir, path.parent().map(Path::to_path_buf));
                let result = self
                    .run_module_initializers(env, &loaded.type_schema_initializers, None)
                    .await;
                self.source_dir = saved_source;
                staged_states.insert(path, result?);
            }
            if !bound_import && self.initialized_schema_count(&staged_states) == before {
                break;
            }
        }
        // Namespace dictionaries are projected after the schema bindings settle.
        let mut namespace_scopes = HashSet::new();
        for import in imports.iter().filter(|import| import.namespace.is_some()) {
            let namespace = import.namespace.as_ref().unwrap();
            let alias = &namespace.alias;
            if namespace.binding == NamespaceImportBinding::Deferred
                && staged_states[&import.importer].contains(alias)
            {
                return Err(VmError::Runtime(format!(
                    "Import collision: '{alias}' is already defined when importing {}. \
                     Use a different namespace alias: import * as <name> from \"...\"",
                    import.target.display()
                )));
            }
            {
                let target = &self.module_cache[&import.target];
                let dict = build_namespace_dict_with_state(
                    &import.target.display().to_string(),
                    target,
                    namespace.members.as_deref(),
                    harn_modules::sibling_module_access(&import.importer, &import.target),
                    &staged_states[&import.target],
                )?;
                let env = staged_states.get_mut(&import.importer).unwrap();
                if namespace_scopes.insert(import.importer.clone()) {
                    env.push_scope();
                }
                env.define(alias, dict, false)?;
            }
        }
        for (path, env) in staged_states {
            *self.module_cache[&path]._module_state.lock() = env;
        }
        self.deferred_cyclic_imports = still_pending;
        Ok(())
    }

    fn initialized_schema_count(&self, states: &BTreeMap<PathBuf, VmEnv>) -> usize {
        states
            .iter()
            .map(|(path, env)| {
                self.module_cache[path]
                    .type_schema_initializers
                    .iter()
                    .filter(|init| env.contains(&init.name))
                    .count()
            })
            .sum()
    }
}
