//! Isolate actual tool interception state in current-thread dispatch tests.

use super::*;
use std::marker::PhantomData;
use std::rc::Rc;

/// Saves and restores the owning registries, including on assertion panic.
/// It neither implements interception nor replaces the real dispatch path.
pub(crate) struct ToolInterceptionScope {
    hooks: Vec<RuntimeHook>,
    singleton: Option<PreToolHookFn>,
    prechecks: Vec<Arc<VmClosure>>,
    depth: usize,
    context: Option<super::super::RunExecutionRecord>,
    _thread: PhantomData<Rc<()>>,
}

impl ToolInterceptionScope {
    pub(crate) fn new() -> Self {
        let hooks = RUNTIME_HOOKS.with(|slot| {
            let mut registry = slot.borrow_mut();
            let saved = registry
                .iter()
                .filter(|hook| matches!(hook.event, HookEvent::PreToolUse | HookEvent::PostToolUse))
                .cloned()
                .collect();
            registry.retain(|hook| {
                !matches!(hook.event, HookEvent::PreToolUse | HookEvent::PostToolUse)
            });
            saved
        });
        Self {
            hooks,
            singleton: SINGLETON_PRE_TOOL_HOOK.with(|slot| slot.borrow_mut().take()),
            prechecks: super::super::swap_tool_precheck_stack(Vec::new()),
            depth: super::super::swap_tool_precheck_depth(0),
            context: super::super::current_execution_context(),
            _thread: PhantomData,
        }
    }

    /// Run a real pre-tool callback; `Some` rewrites arguments, `None` allows.
    pub(crate) fn before(
        &self,
        tool: &str,
        callback: impl Fn() -> Option<serde_json::Value> + Send + Sync + 'static,
    ) {
        register_tool_hook(ToolHook {
            pattern: tool.into(),
            pre: Some(Arc::new(move |_, _| match callback() {
                Some(args) => PreToolAction::Modify(args),
                None => PreToolAction::Allow,
            })),
            post: None,
        });
    }

    pub(crate) fn clear_hooks(&self) {
        clear_tool_hooks();
    }

    pub(crate) fn workspace(&self, root: &std::path::Path) {
        super::super::set_thread_execution_context(Some(super::super::RunExecutionRecord {
            cwd: Some(root.to_string_lossy().into_owned()),
            ..Default::default()
        }));
    }

    pub(crate) fn precheck(&self, closure: Arc<VmClosure>) {
        super::super::push_tool_precheck(closure);
    }
}

impl Drop for ToolInterceptionScope {
    fn drop(&mut self) {
        clear_tool_hooks();
        RUNTIME_HOOKS.with(|slot| slot.borrow_mut().extend(std::mem::take(&mut self.hooks)));
        SINGLETON_PRE_TOOL_HOOK.with(|slot| *slot.borrow_mut() = self.singleton.take());
        super::super::swap_tool_precheck_stack(std::mem::take(&mut self.prechecks));
        super::super::swap_tool_precheck_depth(self.depth);
        super::super::set_thread_execution_context(self.context.take());
    }
}

#[tokio::test(flavor = "current_thread")]
async fn interception_scope_restores_existing_state_after_panic() {
    let outer = ToolInterceptionScope::new();
    outer.before("probe", || Some(serde_json::json!({"saved": true})));
    let singleton: PreToolHookFn = Arc::new(|_, _| PreToolAction::Allow);
    set_singleton_pre_tool_hook(Some(singleton.clone()));
    let program =
        harn_parser::check_source_strict("fn probe(request: dict) { return nil }").unwrap();
    let chunk = crate::compiler::Compiler::new().compile(&program).unwrap();
    let precheck = Arc::new(VmClosure {
        func: chunk
            .functions
            .iter()
            .find(|function| function.name.as_str() == "probe")
            .unwrap()
            .clone(),
        env: crate::value::VmEnv::new(),
        source_dir: None,
        module_functions: None,
        module_state: None,
        retained_module_scope: None,
    });
    outer.precheck(precheck.clone());
    super::super::swap_tool_precheck_depth(7);
    outer.workspace(std::path::Path::new("/saved-dispatch-context"));
    let result = std::panic::catch_unwind(|| {
        let inner = ToolInterceptionScope::new();
        assert!(RUNTIME_HOOKS.with(|slot| slot
            .borrow()
            .iter()
            .all(|hook| !matches!(hook.event, HookEvent::PreToolUse | HookEvent::PostToolUse))));
        assert!(singleton_pre_tool_hook().is_none());
        assert!(!super::super::tool_precheck_active());
        assert_eq!(super::super::swap_tool_precheck_depth(0), 0);
        inner.before("probe", || Some(serde_json::json!({"temporary": true})));
        inner.workspace(std::path::Path::new("/temporary-dispatch-context"));
        panic!("exercise unwinding restoration");
    });
    assert!(result.is_err());
    assert!(Arc::ptr_eq(&singleton_pre_tool_hook().unwrap(), &singleton));
    assert!(Arc::ptr_eq(
        &super::super::current_tool_precheck().unwrap(),
        &precheck
    ));
    assert_eq!(super::super::swap_tool_precheck_depth(7), 7);
    assert_eq!(
        super::super::execution_root_path(),
        std::path::Path::new("/saved-dispatch-context")
    );
    let action = run_pre_tool_hooks("probe", &serde_json::json!({}))
        .await
        .unwrap();
    assert!(
        matches!(action, PreToolAction::Modify(args) if args == serde_json::json!({"saved": true}))
    );
}
