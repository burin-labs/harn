use super::*;
use syn::visit::Visit;

pub(super) fn methods(source: &str) -> BTreeSet<String> {
    struct Dispatches(Vec<BTreeSet<String>>);
    impl<'ast> Visit<'ast> for Dispatches {
        fn visit_expr_match(&mut self, node: &'ast syn::ExprMatch) {
            if let syn::Expr::MethodCall(call) = node.expr.as_ref() {
                if call.method == "as_str"
                    && matches!(call.receiver.as_ref(), syn::Expr::Path(path) if path.path.is_ident("method"))
                {
                    let mut methods = BTreeSet::new();
                    for arm in &node.arms {
                        collect_pattern(&arm.pat, &mut methods);
                    }
                    self.0.push(methods);
                    return;
                }
            }
            syn::visit::visit_expr_match(self, node);
        }
    }
    let mut dispatches = Dispatches(Vec::new());
    dispatches.visit_file(&syn::parse_file(source).expect("ACP dispatch Rust parses"));
    assert_eq!(
        dispatches.0.len(),
        1,
        "exactly one ACP method dispatch required"
    );
    dispatches.0.pop().unwrap()
}

fn collect_pattern(pattern: &syn::Pat, methods: &mut BTreeSet<String>) {
    match pattern {
        syn::Pat::Lit(literal) => {
            let syn::Lit::Str(value) = &literal.lit else {
                panic!("ACP method must be a string");
            };
            methods.insert(value.value());
        }
        syn::Pat::Or(pattern) => {
            for case in &pattern.cases {
                collect_pattern(case, methods);
            }
        }
        syn::Pat::Path(path) => {
            let name = path
                .path
                .segments
                .iter()
                .map(|segment| segment.ident.to_string())
                .collect::<Vec<_>>()
                .join("::");
            methods.insert(
                dispatch_arm_constant_value(&name)
                    .unwrap_or_else(|| panic!("unresolved ACP dispatch constant: {name}")),
            );
        }
        syn::Pat::Ident(ident) => {
            let name = ident.ident.to_string();
            methods.insert(
                dispatch_arm_constant_value(&name)
                    .unwrap_or_else(|| panic!("unresolved ACP dispatch constant: {name}")),
            );
        }
        syn::Pat::Wild(_) => {}
        _ => panic!("unsupported ACP dispatch pattern"),
    }
}

#[test]
fn nested_matches_are_not_public_methods() {
    assert_eq!(
        methods(
            r#"fn dispatch(method: String) {
        match method.as_str() {
            "one" | "two" => { match state { State::Prompt(value) => value, _ => None }; },
            HARN_PROVIDER_CATALOG_METHOD => {},
            _ => {},
        }
    }"#
        ),
        BTreeSet::from([
            "one".to_string(),
            "two".to_string(),
            HARN_PROVIDER_CATALOG_METHOD.to_string()
        ])
    );
}

#[test]
#[should_panic(expected = "unresolved ACP dispatch constant")]
fn unknown_public_arm_is_rejected() {
    methods(
        "fn dispatch(method: String) { match method.as_str() { UNKNOWN_METHOD => {}, _ => {} } }",
    );
}

#[test]
#[should_panic(expected = "exactly one ACP method dispatch required")]
fn missing_dispatch_is_not_empty_success() {
    methods("fn dispatch() {}");
}
