//! Production caller census for the run-approval contract, in the fast CI suite.

use std::collections::BTreeMap;

use syn::visit::Visit;

#[derive(Default)]
struct Calls {
    owner: String,
    function: String,
    callers: BTreeMap<String, Vec<String>>,
}

fn test_only(attrs: &[syn::Attribute]) -> bool {
    attrs.iter().any(|attr| {
        attr.path().is_ident("cfg")
            && attr
                .parse_args::<syn::Path>()
                .is_ok_and(|path| path.is_ident("test"))
    })
}

impl<'ast> Visit<'ast> for Calls {
    fn visit_item_impl(&mut self, node: &'ast syn::ItemImpl) {
        if test_only(&node.attrs) {
            return;
        }
        let syn::Type::Path(path) = &*node.self_ty else {
            return;
        };
        let owner = path
            .path
            .segments
            .iter()
            .map(|segment| segment.ident.to_string())
            .collect::<Vec<_>>()
            .join("::");
        let previous = std::mem::replace(&mut self.owner, owner);
        syn::visit::visit_item_impl(self, node);
        self.owner = previous;
    }

    fn visit_impl_item_fn(&mut self, node: &'ast syn::ImplItemFn) {
        if test_only(&node.attrs) {
            return;
        }
        let caller = format!("{}::{}", self.owner, node.sig.ident);
        let previous = std::mem::replace(&mut self.function, caller);
        syn::visit::visit_impl_item_fn(self, node);
        self.function = previous;
    }

    fn visit_item_mod(&mut self, node: &'ast syn::ItemMod) {
        if !test_only(&node.attrs) {
            syn::visit::visit_item_mod(self, node);
        }
    }

    fn visit_item_fn(&mut self, node: &'ast syn::ItemFn) {
        if test_only(&node.attrs) || node.attrs.iter().any(|attr| attr.path().is_ident("test")) {
            return;
        }
        let previous = std::mem::replace(&mut self.function, node.sig.ident.to_string());
        syn::visit::visit_item_fn(self, node);
        self.function = previous;
    }

    fn visit_expr_call(&mut self, node: &'ast syn::ExprCall) {
        if let syn::Expr::Path(path) = &*node.func {
            let name = path
                .path
                .segments
                .iter()
                .map(|segment| segment.ident.to_string())
                .collect::<Vec<_>>()
                .join("::");
            self.callers
                .entry(self.function.clone())
                .or_default()
                .push(name);
        }
        syn::visit::visit_expr_call(self, node);
    }
}

fn calls(source: &str) -> Calls {
    let mut calls = Calls::default();
    calls.visit_file(&syn::parse_file(source).expect("Rust source parses"));
    calls
}

fn has_call(census: &Calls, caller: &str, required: &str) -> bool {
    census.callers.get(caller).is_some_and(|observed| {
        observed
            .iter()
            .any(|name| name == required || name.ends_with(&format!("::{required}")))
    })
}

fn dispatch_reaches_typed_owner(dispatch: &Calls, approval: &Calls) -> bool {
    has_call(
        dispatch,
        "host_agent_dispatch_tool_call",
        "DispatchApproval::new",
    ) && has_call(
        approval,
        "DispatchApproval::new",
        "current_run_approval_policy",
    ) && !has_call(approval, "DispatchApproval::new", "current_approval_policy")
}

#[test]
fn production_construction_and_dispatch_share_the_typed_owner() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../harn-vm/src");
    let dispatch =
        calls(&std::fs::read_to_string(root.join("llm/agent_host_primitives.rs")).unwrap());
    let approval = calls(
        &std::fs::read_to_string(root.join("llm/agent_host_primitives/dispatch_approval.rs"))
            .unwrap(),
    );
    assert!(
        dispatch_reaches_typed_owner(&dispatch, &approval),
        "production dispatch must construct the invocation-bound approval that reads the typed owner"
    );
    for (path, caller, required) in [
        (
            "orchestration/policy/run_approval.rs",
            "construct_live_approval_policy",
            "RunApprovalPolicy::construct_with_resolver",
        ),
        (
            "stdlib/git/approval.rs",
            "enforce_git_approval",
            "current_run_approval_policy",
        ),
    ] {
        let census = calls(&std::fs::read_to_string(root.join(path)).unwrap());
        let observed = census
            .callers
            .get(caller)
            .expect("production caller exists");
        assert!(
            has_call(&census, caller, required),
            "{path}:{caller} has no {required}; observed {observed:?}"
        );
        if required == "current_run_approval_policy" {
            assert!(
                !has_call(&census, caller, "current_approval_policy"),
                "{path}:{caller} bypasses the typed owner"
            );
        }
    }

    // A constructor that exists only under cfg(test) is no production caller.
    let control = calls("#[cfg(test)] mod tests { fn construct_live_approval_policy() { RunApprovalPolicy::construct_with_resolver(); } }");
    assert!(control.callers.is_empty());
    let positive = calls(
        "fn construct_live_approval_policy() { RunApprovalPolicy::construct_with_resolver(); }",
    );
    assert_eq!(positive.callers["construct_live_approval_policy"].len(), 1);

    // Follow the real constructor edge, with its owner, rather than accepting
    // an unrelated `new` method or a test-only typed-policy lookup.
    let methods = calls(
        "impl DispatchApproval { fn new() { current_run_approval_policy(); } }
         impl OtherApproval { fn new() { current_approval_policy(); } }
         #[cfg(test)] impl HiddenApproval { fn new() { current_run_approval_policy(); } }
         impl TestMethod { #[cfg(test)] fn new() { current_run_approval_policy(); } }",
    );
    assert_eq!(
        methods.callers["DispatchApproval::new"],
        ["current_run_approval_policy"]
    );
    assert_eq!(
        methods.callers["OtherApproval::new"],
        ["current_approval_policy"]
    );
    assert!(!methods.callers.contains_key("HiddenApproval::new"));
    assert!(!methods.callers.contains_key("TestMethod::new"));
    assert!(!methods.callers.contains_key("new"));

    let connected = calls("fn host_agent_dispatch_tool_call() { DispatchApproval::new(); }");
    assert!(dispatch_reaches_typed_owner(&connected, &methods));
    for disconnected in [
        "fn host_agent_dispatch_tool_call() {} fn unused() { DispatchApproval::new(); }",
        "fn host_agent_dispatch_tool_call() { OtherDispatchApproval::new(); }",
    ] {
        assert!(!dispatch_reaches_typed_owner(
            &calls(disconnected),
            &methods
        ));
    }
    for wrong_owner in [
        "impl OtherApproval { fn new() { current_run_approval_policy(); } }",
        "impl DispatchApproval { fn new() {} fn unused() { current_run_approval_policy(); } }",
        "impl DispatchApproval { fn new() { current_approval_policy(); } }",
    ] {
        assert!(!dispatch_reaches_typed_owner(
            &connected,
            &calls(wrong_owner)
        ));
    }
}
