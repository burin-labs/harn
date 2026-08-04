//! Boundary-value and long-running execution safety helpers.

use harn_lexer::Span;
use harn_parser::{DiagnosticCode as Code, Node, SNode};

use super::Linter;
use crate::diagnostic::{LintDiagnostic, LintSeverity};

impl Linter<'_> {
    pub(super) fn has_interpolation(node: &SNode) -> bool {
        use harn_lexer::StringSegment;
        matches!(&node.node, Node::InterpolatedString(segments) if segments.iter().any(|segment| matches!(segment, StringSegment::Expression(_, _, _))))
    }

    pub(super) fn is_boundary_api(name: &str) -> bool {
        matches!(
            name,
            "json_parse"
                | "json_extract"
                | "yaml_parse"
                | "toml_parse"
                | "llm_call"
                | "llm_completion"
                | "http_get"
                | "http_post"
                | "http_put"
                | "http_patch"
                | "http_delete"
                | "http_request"
                | "http_session_request"
                | "sse_receive"
                | "sse_server_mock_receive"
                | "sse_server_response"
                | "sse_server_status"
                | "websocket_accept"
                | "websocket_receive"
                | "host_call"
                | "mcp_call"
        )
    }

    pub(super) fn root_var_name(node: &SNode) -> Option<String> {
        match &node.node {
            Node::Identifier(name) => Some(name.clone()),
            Node::PropertyAccess { object, .. }
            | Node::OptionalPropertyAccess { object, .. }
            | Node::SubscriptAccess { object, .. }
            | Node::OptionalSubscriptAccess { object, .. }
            | Node::SliceAccess { object, .. } => Self::root_var_name(object),
            _ => None,
        }
    }

    pub(super) fn is_secret_scan_call(name: &str, args: &[SNode]) -> bool {
        name == "secret_scan"
            || matches!(
                (name, args.get(1).and_then(Self::string_literal_value)),
                ("mcp_call", Some("harn.secret_scan" | "harn::secret_scan"))
            )
            || matches!(
                (name, args.first().and_then(Self::string_literal_value)),
                (
                    "host_tool_call",
                    Some("harn.secret_scan" | "harn::secret_scan")
                )
            )
    }

    pub(super) fn is_pr_open_call(name: &str, args: &[SNode]) -> bool {
        matches!(
            (name, args.get(1).and_then(Self::string_literal_value)),
            (
                "mcp_call",
                Some("git::push_pr" | "git.push_pr" | "create_pr")
            )
        ) || matches!(
            (name, args.first().and_then(Self::string_literal_value)),
            (
                "host_tool_call",
                Some("git::push_pr" | "git.push_pr" | "create_pr")
            )
        )
    }

    pub(super) fn string_literal_value(node: &SNode) -> Option<&str> {
        match &node.node {
            Node::StringLiteral(value) | Node::RawStringLiteral(value) => Some(value.as_str()),
            _ => None,
        }
    }

    pub(super) fn enter_long_running_body(&mut self, body: &[SNode]) {
        self.long_running_cleanup_stack
            .push(Self::body_has_long_running_cleanup(body));
    }

    pub(super) fn exit_long_running_body(&mut self) {
        self.long_running_cleanup_stack.pop();
    }

    pub(super) fn current_body_has_long_running_cleanup(&self) -> bool {
        self.long_running_cleanup_stack
            .last()
            .copied()
            .unwrap_or(false)
    }

    pub(super) fn warn_unmanaged_long_running_call(&mut self, name: &str, span: Span) {
        if self.current_body_has_long_running_cleanup() {
            return;
        }
        self.diagnostics.push(LintDiagnostic {
            code: Code::LintLongRunningWithoutCleanup,
            rule: "long-running-without-cleanup".into(),
            message: format!("`{name}` starts long-running work without a defer/finally cleanup path"),
            span,
            severity: LintSeverity::Warning,
            suggestion: Some("store the returned handle and cancel it from a defer or finally block with `tools.cancel_handle`".to_string()),
            fix: None,
        });
    }

    pub(super) fn call_uses_background_flag(name: &str, args: &[SNode]) -> bool {
        Self::long_running_capable_call(name, args)
            && args.iter().any(Self::expr_has_background_flag)
    }

    fn long_running_capable_call(name: &str, args: &[SNode]) -> bool {
        matches!(
            name,
            "walk_dir"
                | "glob"
                | "find_text"
                | "http_get"
                | "http_request"
                | "http_download"
                | "run_command"
                | "run_test"
                | "run_build_command"
        ) || matches!(
            (name, args.first().and_then(Self::string_literal_value)),
            (
                "host_tool_call",
                Some(
                    "run_command"
                        | "run_test"
                        | "run_build_command"
                        | "tools.run_command"
                        | "tools.run_test"
                        | "tools.run_build_command"
                )
            )
        )
    }

    fn expr_has_background_flag(node: &SNode) -> bool {
        matches!(&node.node, Node::DictLiteral(entries) if entries.iter().any(|entry| Self::dict_key_name(&entry.key).as_deref() == Some("background") && matches!(entry.value.node, Node::BoolLiteral(true))))
    }

    fn body_has_long_running_cleanup(body: &[SNode]) -> bool {
        body.iter().any(Self::node_has_long_running_cleanup)
    }

    fn node_has_long_running_cleanup(node: &SNode) -> bool {
        match &node.node {
            Node::DeferStmt { body } => Self::block_calls_cancel_handle(body),
            Node::TryCatch {
                body,
                catch_body,
                finally_body,
                ..
            } => {
                finally_body
                    .as_ref()
                    .is_some_and(|body| Self::block_calls_cancel_handle(body))
                    || Self::body_has_long_running_cleanup(body)
                    || Self::body_has_long_running_cleanup(catch_body)
            }
            Node::IfElse {
                then_body,
                else_body,
                ..
            } => {
                Self::body_has_long_running_cleanup(then_body)
                    || else_body
                        .as_ref()
                        .is_some_and(|body| Self::body_has_long_running_cleanup(body))
            }
            Node::ForIn { body, .. }
            | Node::WhileLoop { body, .. }
            | Node::Retry { body, .. }
            | Node::CostRoute { body, .. }
            | Node::Block(body)
            | Node::SpawnExpr { body }
            | Node::ScopeBlock { body }
            | Node::Closure { body, .. } => Self::body_has_long_running_cleanup(body),
            _ => false,
        }
    }

    fn block_calls_cancel_handle(body: &[SNode]) -> bool {
        body.iter().any(Self::node_calls_cancel_handle)
    }

    fn node_calls_cancel_handle(node: &SNode) -> bool {
        if let Node::FunctionCall { name, args, .. } = &node.node {
            if name == "cancel_handle"
                || matches!(
                    (
                        name.as_str(),
                        args.first().and_then(Self::string_literal_value)
                    ),
                    (
                        "host_tool_call",
                        Some("cancel_handle" | "tools.cancel_handle")
                    )
                )
            {
                return true;
            }
        }
        harn_parser::visit::immediate_children(node)
            .into_iter()
            .any(Self::node_calls_cancel_handle)
    }

    pub(super) fn dict_key_name(node: &SNode) -> Option<String> {
        match &node.node {
            Node::Identifier(value)
            | Node::StringLiteral(value)
            | Node::RawStringLiteral(value) => Some(value.clone()),
            _ => None,
        }
    }
}
