use super::*;

impl LoadedModule {
    pub(super) fn has_pending_type_schemas(&self) -> bool {
        let state = self._module_state.lock();
        self.type_schema_initializers
            .iter()
            .any(|init| !state.contains(&init.name))
    }

    pub(crate) fn public_type_schema(&self, name: &str) -> Option<VmValue> {
        self.public_type_names
            .contains(name)
            .then(|| self._module_state.lock().get(name))
            .flatten()
    }
}

impl Vm {
    pub(super) async fn run_module_initializers(
        &mut self,
        env: VmEnv,
        schemas: &[crate::compiler::TypeSchemaInitializer<Arc<crate::chunk::Chunk>>],
        body: Option<&Arc<crate::chunk::Chunk>>,
    ) -> Result<VmEnv, VmError> {
        let saved_env = std::mem::replace(&mut self.env, env);
        let saved_frames = std::mem::take(&mut self.frames);
        let saved_handlers = std::mem::take(&mut self.exception_handlers);
        let saved_iterators = std::mem::take(&mut self.iterators);
        let saved_deadlines = std::mem::take(&mut self.deadlines);
        let saved_sync_guards = std::mem::take(&mut self.held_sync_guards);
        let saved_task_scopes = std::mem::take(&mut self.task_scopes);
        // Module frames must not prune the caller's active steps or personas.
        let active_context = self.runtime.suspend_active_context();
        let result: Result<(), VmError> = async {
            for init in schemas {
                if self.env.get(&init.name).is_none()
                    && init
                        .required_bindings
                        .iter()
                        .all(|name| self.env.get(name).is_some())
                {
                    self.run_chunk(Arc::clone(&init.chunk)).await?;
                }
            }
            if let Some(chunk) = body {
                self.run_chunk(Arc::clone(chunk)).await?;
            }
            Ok(())
        }
        .await;
        drop(active_context);
        let initialized_env = std::mem::replace(&mut self.env, saved_env);
        self.frames = saved_frames;
        self.exception_handlers = saved_handlers;
        self.iterators = saved_iterators;
        self.deadlines = saved_deadlines;
        self.held_sync_guards.clear();
        self.cancel_task_scopes_where(|_| true);
        self.held_sync_guards = saved_sync_guards;
        self.task_scopes = saved_task_scopes;
        result?;
        Ok(initialized_env)
    }
}
