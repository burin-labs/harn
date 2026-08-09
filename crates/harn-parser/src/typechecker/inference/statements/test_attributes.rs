use super::*;

impl TypeChecker {
    pub(super) fn validate_test_attribute_target(&mut self, attr: &Attribute, inner: &SNode) {
        let valid = match attr.name.as_str() {
            "test" => matches!(inner.node, Node::Pipeline { .. }),
            "test_fixture" => matches!(inner.node, Node::FnDecl { .. }),
            _ => return,
        };
        if !valid {
            self.warning_at(
                Code::InvalidAttributeTarget,
                format!(
                    "`@{}` only applies to {} declarations",
                    attr.name,
                    if attr.name == "test" {
                        "pipeline"
                    } else {
                        "function"
                    }
                ),
                attr.span,
            );
        }
    }

    pub(super) fn validate_test_args(&mut self, attr: &Attribute) {
        const KNOWN_KEYS: &[&str] = &["cases", "fixture"];
        for arg in &attr.args {
            let Some(name) = self.require_named_arg("@test", arg) else {
                continue;
            };
            if !KNOWN_KEYS.contains(&name) {
                self.warning_at(
                    Code::InvalidAttributeArgument,
                    format!("unknown `@test` argument `{name}`; expected one of {KNOWN_KEYS:?}"),
                    arg.span,
                );
                continue;
            }
            match name {
                "cases" if !matches!(arg.value.node, Node::ListLiteral(_)) => self.warning_at(
                    Code::InvalidAttributeArgument,
                    "`@test(cases: ...)` must be a list of `{name, args}` rows".to_string(),
                    arg.span,
                ),
                "fixture" => {
                    self.expect_symbol_like("@test", name, &arg.value, arg.span);
                }
                _ => {}
            }
        }
    }

    pub(super) fn validate_test_fixture_args(&mut self, attr: &Attribute) {
        if attr.args.len() != 1 {
            self.warning_at(
                Code::InvalidAttributeArgument,
                "`@test_fixture` requires exactly one `scope: file|case` argument".to_string(),
                attr.span,
            );
        }
        for arg in &attr.args {
            let Some(name) = self.require_named_arg("@test_fixture", arg) else {
                continue;
            };
            if name != "scope" {
                self.warning_at(
                    Code::InvalidAttributeArgument,
                    format!("unknown `@test_fixture` argument `{name}`; expected `scope`"),
                    arg.span,
                );
                continue;
            }
            self.expect_one_of(
                "@test_fixture",
                "scope",
                &arg.value,
                arg.span,
                &["file", "case"],
            );
        }
    }
}
