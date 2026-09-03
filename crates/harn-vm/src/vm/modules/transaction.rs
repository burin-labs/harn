use super::*;

impl Vm {
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
    pub(super) fn flush_deferred_cyclic_imports(&mut self) -> Result<(), VmError> {
        if self.deferred_cyclic_imports.is_empty() {
            return Ok(());
        }
        let deferred = std::mem::take(&mut self.deferred_cyclic_imports);
        let mut still_pending = Vec::new();
        let mut staged_states: BTreeMap<PathBuf, (crate::value::ModuleState, VmEnv)> =
            BTreeMap::new();
        for import in deferred {
            let (Some(importer), Some(target)) = (
                self.module_cache.get(&import.importer).cloned(),
                self.module_cache.get(&import.target).cloned(),
            ) else {
                still_pending.push(import);
                continue;
            };

            let (_, module_state) =
                staged_states
                    .entry(import.importer.clone())
                    .or_insert_with(|| {
                        let state = Arc::clone(&importer._module_state);
                        let env = state.lock().clone();
                        (state, env)
                    });
            if let Some(alias) = &import.namespace_alias {
                if module_state.get(alias).is_none() {
                    let dict = build_namespace_dict(
                        &import.target.display().to_string(),
                        &target,
                        import.namespace_members.as_deref(),
                    )?;
                    module_state.define(alias, dict, false)?;
                }
                continue;
            }

            let export_names = module_import_names(
                &import.target.display().to_string(),
                &target,
                import.selected_names.as_deref(),
                ImportNameUse::Binding,
            )?;
            for name in export_names {
                if module_state.get(&name).is_some() {
                    continue;
                }
                if let Some(closure) = target.functions.get(&name) {
                    module_state.define(&name, VmValue::Closure(Arc::clone(closure)), false)?;
                } else if let Some(value) = target.public_values.get(&name) {
                    module_state.define(&name, value.clone(), false)?;
                } else if target
                    .public_exports
                    .get(&name)
                    .is_some_and(|kind| !kind.has_runtime_value())
                {
                    continue;
                } else {
                    return Err(VmError::Runtime(format!(
                        "Import error: '{name}' is not defined in {}",
                        import.target.display()
                    )));
                }
            }
        }

        for (_, (state, staged_env)) in staged_states {
            *state.lock() = staged_env;
        }
        self.deferred_cyclic_imports = still_pending;
        Ok(())
    }
}
