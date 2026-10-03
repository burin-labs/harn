//! Production caller census for the run-approval contract, in the fast CI suite.

use std::collections::BTreeMap;

use syn::visit::Visit;

#[derive(Default)]
struct Calls {
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

#[test]
fn production_construction_and_dispatch_share_the_typed_owner() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../harn-vm/src");
    for (path, caller, required) in [
        (
            "orchestration/policy/run_approval.rs",
            "construct_live_approval_policy",
            "RunApprovalPolicy::construct_with_resolver",
        ),
        (
            "llm/agent_host_primitives.rs",
            "host_agent_dispatch_tool_call",
            "current_run_approval_policy",
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
            observed.iter().any(|name| name.ends_with(required)),
            "{path}:{caller} has no {required}; observed {observed:?}"
        );
        if required == "current_run_approval_policy" {
            assert!(
                !observed
                    .iter()
                    .any(|name| name.ends_with("current_approval_policy")),
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
}
