//! The stateful linter walk: a [`Linter`] collects diagnostics while
//! traversing the AST, then finalizes post-walk checks
//! (unused/undefined symbols, etc.). The large `lint_node` match lives
//! in the [`walk`] submodule.

use std::collections::{HashMap, HashSet};

use harn_lexer::{FixEdit, Span};
use harn_parser::diagnostic::{
    find_closest_match, harness_clock_replacement, harness_env_replacement, harness_fs_replacement,
    harness_net_replacement, harness_random_replacement, harness_stdio_replacement,
    renamed_stdlib_symbol,
};
use harn_parser::{BindingPattern, DiagnosticCode as Code, Node, SNode, TypeExpr, TypedParam};

use crate::complexity::cyclomatic_complexity;
use crate::decls::{Declaration, FnDeclaration, ImportInfo, ParamDeclaration, TypeDeclaration};
use crate::diagnostic::{LintDiagnostic, LintSeverity, DEFAULT_COMPLEXITY_THRESHOLD};
use crate::fixes::{
    append_sink_fix, is_pure_expression, remove_method_call_wrapper_fix,
    replace_identifier_text_fix, simple_ident_rename_fix,
};
use crate::harndoc::check_legacy_doc_comments;
use crate::naming::{is_pascal_case, is_snake_case, to_pascal_case, to_snake_case};
use crate::rules::blank_lines::check_blank_line_between_items;
use crate::rules::deprecated_llm_options::check_deprecated_llm_options;
use crate::rules::import_order::check_import_order;
use crate::rules::optional_shorthand::check_prefer_optional_shorthand;
use crate::rules::reminder_lifecycle::check_reminder_lifecycle_literals;
use crate::rules::reminder_provider_count::check_reminder_provider_count;
use crate::rules::reminder_role_hint::check_reminder_role_hint_capabilities;
use crate::rules::trailing_comma::check_trailing_comma;
use crate::rules::unnecessary_parentheses::check_unnecessary_parentheses;

mod walk;

/// Inputs threaded into [`Linter::check_ambient_capability_builtin`].
/// One value per ambient call site keeps the per-sub-handle wrappers
/// (`check_ambient_clock_builtin`, etc.) tiny: they only carry the
/// per-capability replacement table, diagnostic code, rule name, and
/// the `require_harness_in_scope` switch.
struct AmbientCapabilityLint<'a> {
    name: &'a str,
    span: Span,
    replacement: Option<&'static str>,
    code: Code,
    rule: &'static str,
    sub_handle: &'static str,
    require_harness_in_scope: bool,
}

fn is_explicit_seeded_random_call(name: &str, arg_count: usize) -> bool {
    matches!(
        (name, arg_count),
        ("random", 1) | ("random_int", 3) | ("random_choice", 2) | ("random_shuffle", 2)
    )
}

/// The linter walks the AST and collects diagnostics.
pub(crate) struct Linter<'a> {
    pub(super) diagnostics: Vec<LintDiagnostic>,
    pub(super) scopes: Vec<HashSet<String>>,
    pub(super) declarations: Vec<Declaration>,
    pub(super) param_declarations: Vec<ParamDeclaration>,
    pub(super) references: HashSet<String>,
    pub(super) assignments: HashSet<String>,
    pub(super) imports: Vec<ImportInfo>,
    /// Track whether we are inside a loop (for break/continue validation).
    pub(super) loop_depth: usize,
    /// Track all declared/known function names for undefined-function detection.
    pub(super) known_functions: HashSet<String>,
    /// Builtin function names derived from the live VM stdlib.
    pub(super) builtin_functions: HashSet<String>,
    /// Track function call sites for undefined-function checking.
    pub(super) function_calls: Vec<(String, Span)>,
    /// Function names declared with `@step`.
    pub(super) step_functions: HashSet<String>,
    /// Static step name declared for each `@step` function.
    pub(super) step_names_by_function: HashMap<String, String>,
    /// Static step names called by each `@persona` function.
    pub(super) persona_steps: HashMap<String, HashSet<String>>,
    /// Function calls made from `@persona` bodies.
    pub(super) persona_body_calls: Vec<(String, Span, String)>,
    /// Opt-in function names allowed directly inside `@persona` bodies.
    pub(super) persona_step_allowlist: HashSet<String>,
    /// Whether the file has wildcard imports (import "module").
    /// If true, skip undefined-function checks since we can't know what was imported.
    pub(super) has_wildcard_import: bool,
    /// Whether wildcard imports were resolved using [`harn_modules`] and we can
    /// choose between known/wildcard modes explicitly.
    pub(crate) use_module_graph_for_wildcards: bool,
    /// Wildcard export names resolved from [`harn_modules`]. `None` means
    /// unknown, so conservative behavior should skip undefined-function checks.
    pub(crate) module_graph_wildcard_exports: Option<HashSet<String>>,
    /// Track function declarations for unused-function detection.
    pub(super) fn_declarations: Vec<FnDeclaration>,
    /// Track actual function usage sites (calls + value references).
    /// Separate from `references` so FnDecl doesn't self-count.
    pub(super) function_references: HashSet<String>,
    /// Whether the current function is inside an impl block.
    pub(super) in_impl_block: bool,
    pub(super) source: Option<&'a str>,
    /// Function names imported by other files (cross-module analysis).
    /// Functions in this set are not flagged as unused even if they have
    /// no local references, because another file explicitly imports them.
    pub(crate) externally_imported_names: HashSet<String>,
    /// Track whether the current traversal is inside a test pipeline body.
    pub(super) test_pipeline_depth: usize,
    /// Track type declarations for the `unused-type` lint rule.
    pub(super) type_declarations: Vec<TypeDeclaration>,
    /// Track type names referenced anywhere in the file.
    pub(super) type_references: HashSet<String>,
    /// Stack of declared return types for the current function nesting.
    /// Used by the `eager-collection-conversion` lint rule to flag
    /// `return <iter-chain>` inside a function declared to return a
    /// concrete collection.
    pub(super) return_type_stack: Vec<Option<TypeExpr>>,
    /// Stack of typed harness parameter names for the current callable.
    /// A local binding named `harness` is not enough to safely rewrite
    /// capability calls to `harness.*`.
    pub(super) harness_param_stack: Vec<Option<String>>,
    /// Tracks how many enclosing `@complexity(allow)` attributes are
    /// active. When > 0, the cyclomatic-complexity rule is suppressed
    /// for the contained function.
    pub(super) complexity_suppression_depth: usize,
    /// Threshold above which the cyclomatic-complexity rule fires.
    /// Configurable via `[lint].complexity_threshold` in `harn.toml`.
    pub(crate) complexity_threshold: usize,
    /// Suppress the discarded-approval-result lint for the final expression
    /// in value-producing blocks such as `try { ... }`.
    pub(super) value_block_depth: usize,
    /// Stack of connector exports whose default effect policy restricts
    /// direct builtin calls.
    pub(super) connector_effect_export_stack: Vec<String>,
    /// Whether the current body contains a defer/finally path that cancels
    /// long-running handles.
    pub(super) long_running_cleanup_stack: Vec<bool>,
    /// Registry variables mapped to `tool_define` calls that will need MCP
    /// annotations if the registry is later exposed through `mcp_tools`.
    pub(super) mcp_registry_missing_annotation_spans: HashMap<String, Vec<Span>>,
    /// When set, public `fn` declarations must carry the structured
    /// `@effects` / `@allocation` / `@errors` / `@api_stability` /
    /// `@example` block. Enabled by callers linting stdlib sources.
    pub(crate) require_stdlib_metadata: bool,
    /// Lazily-lexed comment tokens, cached so the `missing-harndoc`
    /// suppression gate (which runs per public item) does not re-tokenize
    /// the whole source each time.
    pub(super) cached_comment_toks: Option<Vec<crate::harndoc::LegacyCommentTok>>,
}

impl<'a> Linter<'a> {
    pub(crate) fn new(source: Option<&'a str>) -> Self {
        Self {
            diagnostics: Vec::new(),
            scopes: vec![HashSet::new()],
            declarations: Vec::new(),
            param_declarations: Vec::new(),
            references: HashSet::new(),
            assignments: HashSet::new(),
            imports: Vec::new(),
            loop_depth: 0,
            known_functions: Self::builtin_names(),
            builtin_functions: Self::builtin_names(),
            function_calls: Vec::new(),
            step_functions: HashSet::new(),
            step_names_by_function: HashMap::new(),
            persona_steps: HashMap::new(),
            persona_body_calls: Vec::new(),
            persona_step_allowlist: HashSet::new(),
            has_wildcard_import: false,
            use_module_graph_for_wildcards: false,
            module_graph_wildcard_exports: None,
            fn_declarations: Vec::new(),
            function_references: HashSet::new(),
            in_impl_block: false,
            source,
            externally_imported_names: HashSet::new(),
            test_pipeline_depth: 0,
            type_declarations: Vec::new(),
            type_references: HashSet::new(),
            return_type_stack: Vec::new(),
            harness_param_stack: Vec::new(),
            complexity_suppression_depth: 0,
            complexity_threshold: DEFAULT_COMPLEXITY_THRESHOLD,
            value_block_depth: 0,
            connector_effect_export_stack: Vec::new(),
            long_running_cleanup_stack: Vec::new(),
            mcp_registry_missing_annotation_spans: HashMap::new(),
            require_stdlib_metadata: false,
            cached_comment_toks: None,
        }
    }

    /// Whether a public item on `item_line` is preceded by a wrong-format
    /// comment run (`//` / `///` or a plain `/* */` block) that the
    /// `legacy-doc-comment` rule will migrate to `/** */`. When true, the
    /// `missing-harndoc` diagnostic is suppressed so the user sees a single
    /// auto-fixable finding instead of a fixless "missing" alongside it.
    pub(super) fn has_adjacent_migratable_comment(&mut self, item_line: usize) -> bool {
        let Some(source) = self.source else {
            return false;
        };
        if self.cached_comment_toks.is_none() {
            self.cached_comment_toks = Some(crate::harndoc::collect_comment_tokens(source));
        }
        let comments = self.cached_comment_toks.as_deref().unwrap_or(&[]);
        crate::harndoc::run_above_is_migratable(comments, item_line)
    }

    /// Return set of known builtin function names, derived from the VM's
    /// live stdlib registration so there is no separate list to maintain.
    fn builtin_names() -> HashSet<String> {
        harn_vm::stdlib::stdlib_builtin_names()
            .into_iter()
            .collect()
    }

    pub(super) fn push_scope(&mut self) {
        self.scopes.push(HashSet::new());
    }

    pub(super) fn pop_scope(&mut self) {
        self.scopes.pop();
    }

    pub(super) fn in_test_pipeline(&self) -> bool {
        self.test_pipeline_depth > 0
    }

    pub(super) fn is_test_pipeline_name(name: &str) -> bool {
        name == "test" || name.starts_with("test_")
    }

    pub(super) fn is_entry_pipeline_name(name: &str) -> bool {
        matches!(name, "default" | "main" | "auto")
    }

    pub(super) fn is_assert_builtin(name: &str) -> bool {
        matches!(name, "assert" | "assert_eq" | "assert_ne")
    }

    pub(super) fn is_approval_record_builtin(name: &str) -> bool {
        name == "request_approval"
    }

    pub(super) fn check_renamed_stdlib_symbol(&mut self, name: &str, span: Span) {
        if harness_stdio_replacement(name).is_some() {
            return;
        }
        let Some(replacement) = renamed_stdlib_symbol(name) else {
            return;
        };
        self.diagnostics.push(LintDiagnostic {
            code: Code::LintRenamedStdlibSymbol,
            rule: "renamed-stdlib-symbol",
            message: format!("`{name}` was renamed to `{replacement}`"),
            span,
            severity: LintSeverity::Warning,
            suggestion: Some(format!("replace `{name}` with `{replacement}`")),
            fix: replace_identifier_text_fix(self.source, span, name, replacement),
        });
    }

    /// Flag ambient clock builtins (`now_ms`, `monotonic_ms`, `sleep_ms`,
    /// `timestamp`, `elapsed`) so the E4.3 → E4.6 migration can rewrite
    /// them to `harness.clock.*`. Direct lint fixes only run when a
    /// `harness` (or `_harness`) binding is in scope; otherwise `harn fix`
    /// chooses between global-harness rewrites and explicit parameter
    /// threading.
    pub(super) fn check_ambient_clock_builtin(&mut self, name: &str, span: Span) {
        self.check_ambient_capability_builtin(AmbientCapabilityLint {
            name,
            span,
            replacement: harness_clock_replacement(name),
            code: Code::LintAmbientClockBuiltin,
            rule: "ambient-clock-builtin",
            sub_handle: "clock",
            require_harness_in_scope: false,
        });
    }

    /// Flag ambient stdio builtins (`print`, `println`, `eprint`,
    /// `eprintln`, `read_line`, `prompt_user`) so the E4.2 → E4.6
    /// migration can rewrite them to `harness.stdio.*`.
    pub(super) fn check_ambient_stdio_builtin(&mut self, name: &str, span: Span) {
        self.check_ambient_capability_builtin(AmbientCapabilityLint {
            name,
            span,
            replacement: harness_stdio_replacement(name),
            code: Code::LintAmbientStdioBuiltin,
            rule: "ambient-stdio-builtin",
            sub_handle: "stdio",
            require_harness_in_scope: false,
        });
    }

    /// Flag ambient fs builtins (`read_file`, `write_file`, ...) so the
    /// E4.4 → E4.6 migration can rewrite them to `harness.fs.*`.
    pub(super) fn check_ambient_fs_builtin(&mut self, name: &str, span: Span) {
        self.check_ambient_capability_builtin(AmbientCapabilityLint {
            name,
            span,
            replacement: harness_fs_replacement(name),
            code: Code::LintAmbientFsBuiltin,
            rule: "ambient-fs-builtin",
            sub_handle: "fs",
            require_harness_in_scope: false,
        });
    }

    /// Flag ambient env builtins (`env`, `env_or`) so the E4.4 → E4.6
    /// migration can rewrite them to `harness.env.*`.
    pub(super) fn check_ambient_env_builtin(&mut self, name: &str, span: Span) {
        self.check_ambient_capability_builtin(AmbientCapabilityLint {
            name,
            span,
            replacement: harness_env_replacement(name),
            code: Code::LintAmbientEnvBuiltin,
            rule: "ambient-env-builtin",
            sub_handle: "env",
            require_harness_in_scope: false,
        });
    }

    /// Flag ambient random builtins (`random`, `random_int`,
    /// `random_choice`, `random_shuffle`) so the E4.4 → E4.6 migration
    /// can rewrite them to `harness.random.*`. Calls that receive an
    /// explicit seeded Rng handle are deterministic data operations, not
    /// ambient host-random access.
    pub(super) fn check_ambient_random_builtin(
        &mut self,
        name: &str,
        arg_count: usize,
        span: Span,
    ) {
        if is_explicit_seeded_random_call(name, arg_count) {
            return;
        }
        self.check_ambient_capability_builtin(AmbientCapabilityLint {
            name,
            span,
            replacement: harness_random_replacement(name),
            code: Code::LintAmbientRandomBuiltin,
            rule: "ambient-random-builtin",
            sub_handle: "random",
            require_harness_in_scope: false,
        });
    }

    /// Flag ambient net builtins (`http_get`, `http_post`, ...) so the
    /// E4.4 → E4.6 migration can rewrite them to `harness.net.*`.
    pub(super) fn check_ambient_net_builtin(&mut self, name: &str, span: Span) {
        self.check_ambient_capability_builtin(AmbientCapabilityLint {
            name,
            span,
            replacement: harness_net_replacement(name),
            code: Code::LintAmbientNetBuiltin,
            rule: "ambient-net-builtin",
            sub_handle: "net",
            require_harness_in_scope: false,
        });
    }

    /// Shared implementation for every ambient-capability lint. The
    /// per-sub-handle wrappers above just supply the replacement table,
    /// diagnostic code, rule slug, and surface name.
    fn check_ambient_capability_builtin(&mut self, lint: AmbientCapabilityLint<'_>) {
        let Some(replacement) = lint.replacement else {
            return;
        };
        if self.has_local_or_imported_name(lint.name) {
            return;
        }
        let harness_binding = self.harness_binding_name();
        if lint.require_harness_in_scope && harness_binding.is_none() {
            return;
        }
        let replacement = harness_binding
            .map(|binding| replacement.replacen("harness", binding, 1))
            .unwrap_or_else(|| replacement.to_string());
        let fix = harness_binding.and_then(|_| {
            replace_identifier_text_fix(self.source, lint.span, lint.name, &replacement)
        });
        let suggestion = if harness_binding.is_some() {
            format!("replace `{}` with `{}`", lint.name, replacement)
        } else {
            format!(
                "run `harn fix --apply --safety scope-local` to call `{replacement}` through the \
                 VM-level `harness` binding, or opt into \
                 `--harness-threading thread-params` for explicit parameter threading"
            )
        };
        self.diagnostics.push(LintDiagnostic {
            code: lint.code,
            rule: lint.rule,
            message: format!(
                "ambient `{}` is deprecated — capabilities now route through `harness.{}.*`",
                lint.name, lint.sub_handle,
            ),
            span: lint.span,
            severity: LintSeverity::Warning,
            suggestion: Some(suggestion),
            fix,
        });
    }

    fn harness_binding_name(&self) -> Option<&str> {
        self.harness_param_stack
            .last()
            .and_then(|name| name.as_deref())
    }

    fn has_local_or_imported_name(&self, name: &str) -> bool {
        self.scopes.iter().any(|scope| scope.contains(name))
            || self
                .imports
                .iter()
                .any(|import| import.names.iter().any(|imported| imported == name))
            || self
                .fn_declarations
                .iter()
                .any(|declaration| declaration.name == name)
    }

    pub(super) fn callable_harness_param(params: &[TypedParam]) -> Option<String> {
        params.iter().find_map(|param| {
            let TypeExpr::Named(name) = param.type_expr.as_ref()? else {
                return None;
            };
            (name == "Harness" && matches!(param.name.as_str(), "harness" | "_harness"))
                .then(|| param.name.clone())
        })
    }
    /// Warn when a public stdlib `pub fn` is missing one or more of the
    /// declared metadata fields (`@effects`, `@allocation`, `@errors`,
    /// `@api_stability`, `@example`). Only runs when callers opted in via
    /// [`crate::LintOptions::require_stdlib_metadata`].
    ///
    /// Functions with no canonical `/** */` block at all are exempt — the
    /// separate `missing-harndoc` lint (HARN-LNT-024) already covers them,
    /// so reporting HARN-STD-101 in that case is redundant noise. Once a
    /// doc block exists, this lint enforces metadata completeness on top.
    pub(super) fn check_stdlib_metadata(&mut self, name: &str, span: Span) {
        let Some(source) = self.source else {
            return;
        };
        let Some(meta) = harn_parser::parse_stdlib_metadata(source, &span) else {
            return;
        };
        if meta.is_complete() {
            return;
        }
        let missing = meta.missing_fields();
        let missing_list = missing.join(", ");
        let message = if meta.is_empty() {
            format!(
                "public stdlib function `{name}` is missing the `@effects`/`@allocation`/`@errors`/`@api_stability`/`@example` metadata block"
            )
        } else {
            format!(
                "public stdlib function `{name}` is missing required metadata fields: {missing_list}"
            )
        };
        self.diagnostics.push(LintDiagnostic {
            code: Code::LintMissingStdlibMetadata,
            rule: "missing-stdlib-metadata",
            message,
            span,
            severity: LintSeverity::Warning,
            suggestion: Some(format!(
                "add the missing fields ({missing_list}) inside the `/** ... */` block above `pub fn {name}`"
            )),
            fix: None,
        });
    }

    /// Score the body of a function/tool and emit a
    /// `cyclomatic-complexity` warning if it exceeds the configured
    /// threshold. No-op when the enclosing decl carries
    /// `@complexity(allow)`.
    pub(super) fn check_cyclomatic_complexity(&mut self, name: &str, body: &[SNode], span: Span) {
        if self.complexity_suppression_depth > 0 {
            return;
        }
        let complexity = cyclomatic_complexity(body);
        let threshold = self.complexity_threshold;
        if complexity <= threshold {
            return;
        }
        self.diagnostics.push(LintDiagnostic {
            code: Code::LintCyclomaticComplexity,
            rule: "cyclomatic-complexity",
            message: format!(
                "function `{name}` has cyclomatic complexity {complexity} (> {threshold})"
            ),
            span,
            severity: LintSeverity::Warning,
            suggestion: Some(format!(
                "split `{name}` into smaller helpers, or mark it `@complexity(allow)` if the branching is intrinsic; threshold configurable via `[lint].complexity_threshold` in `harn.toml`"
            )),
            fix: None,
        });
    }

    pub(super) fn lint_function_name(&mut self, name: &str, span: Span) {
        if is_snake_case(name) {
            return;
        }
        self.diagnostics.push(LintDiagnostic {
            code: Code::LintNamingConvention,
            rule: "naming-convention",
            message: format!("function `{name}` should use snake_case"),
            span,
            severity: LintSeverity::Warning,
            suggestion: Some(format!(
                "rename `{name}` to snake_case (for example `{}`)",
                to_snake_case(name)
            )),
            fix: None,
        });
    }

    pub(super) fn lint_type_name(&mut self, kind: &'static str, name: &str, span: Span) {
        if is_pascal_case(name) {
            return;
        }
        self.diagnostics.push(LintDiagnostic {
            code: Code::LintNamingConvention,
            rule: "naming-convention",
            message: format!("{kind} `{name}` should use PascalCase"),
            span,
            severity: LintSeverity::Warning,
            suggestion: Some(format!(
                "rename `{name}` to PascalCase (for example `{}`)",
                to_pascal_case(name)
            )),
            fix: None,
        });
    }

    pub(super) fn record_type_expr_references(&mut self, type_expr: &TypeExpr) {
        match type_expr {
            TypeExpr::Named(name) => {
                self.type_references.insert(name.clone());
            }
            TypeExpr::Union(types) | TypeExpr::Intersection(types) => {
                for inner in types {
                    self.record_type_expr_references(inner);
                }
            }
            TypeExpr::Shape(fields) => {
                for field in fields {
                    self.record_type_expr_references(&field.type_expr);
                }
            }
            TypeExpr::List(inner) => self.record_type_expr_references(inner),
            TypeExpr::Iter(inner) => self.record_type_expr_references(inner),
            TypeExpr::Generator(inner) => self.record_type_expr_references(inner),
            TypeExpr::Stream(inner) => self.record_type_expr_references(inner),
            TypeExpr::DictType(key, value) => {
                self.record_type_expr_references(key);
                self.record_type_expr_references(value);
            }
            TypeExpr::Applied { name, args } => {
                self.type_references.insert(name.clone());
                for arg in args {
                    self.record_type_expr_references(arg);
                }
            }
            TypeExpr::FnType {
                params,
                return_type,
            } => {
                for param in params {
                    self.record_type_expr_references(param);
                }
                self.record_type_expr_references(return_type);
            }
            TypeExpr::Never => {}
            TypeExpr::LitString(_) | TypeExpr::LitInt(_) => {}
            TypeExpr::Owned(inner) => self.record_type_expr_references(inner),
        }
    }

    /// Map a type annotation to the matching iterator sink method name when
    /// the annotation is a concrete collection type. Returns `None` for
    /// non-collection annotations (including `Iter<T>` itself, which is
    /// already the expression's inferred shape).
    pub(super) fn expected_collection_sink(type_expr: &TypeExpr) -> Option<&'static str> {
        match type_expr {
            TypeExpr::List(_) => Some("to_list"),
            TypeExpr::DictType(_, _) => Some("to_dict"),
            TypeExpr::Applied { name, .. } => match name.as_str() {
                "list" => Some("to_list"),
                "set" => Some("to_set"),
                "dict" => Some("to_dict"),
                _ => None,
            },
            TypeExpr::Named(name) => match name.as_str() {
                "list" => Some("to_list"),
                "set" => Some("to_set"),
                "dict" => Some("to_dict"),
                _ => None,
            },
            _ => None,
        }
    }

    /// Heuristic: does this expression look like a lazy iterator chain that
    /// would yield an `Iter<T>` rather than a concrete collection? We flag
    /// method calls whose outermost (tail) method is a known lazy
    /// combinator or `iter` lift. Sink-terminated chains (e.g.
    /// `...to_list()`) return false.
    pub(super) fn expr_yields_iter(node: &Node) -> bool {
        match node {
            Node::MethodCall { method, .. } | Node::OptionalMethodCall { method, .. } => {
                matches!(
                    method.as_str(),
                    "iter"
                        | "map"
                        | "filter"
                        | "flat_map"
                        | "take"
                        | "skip"
                        | "take_while"
                        | "skip_while"
                        | "zip"
                        | "enumerate"
                        | "chain"
                        | "chunks"
                        | "windows"
                )
            }
            Node::FunctionCall { name, .. } => {
                matches!(name.as_str(), "iter")
            }
            _ => false,
        }
    }

    pub(super) fn constant_logical_reduction(
        op: &str,
        left: &SNode,
        right: &SNode,
    ) -> Option<(String, &'static str)> {
        match (op, &left.node, &right.node) {
            ("||", Node::BoolLiteral(true), _) => Some((
                "`true || expr` always evaluates to `true`; the right side is unreachable"
                    .to_string(),
                "true",
            )),
            ("||", _, Node::BoolLiteral(true)) if is_pure_expression(&left.node) => Some((
                "`expr || true` always evaluates to `true`".to_string(),
                "true",
            )),
            ("&&", Node::BoolLiteral(false), _) => Some((
                "`false && expr` always evaluates to `false`; the right side is unreachable"
                    .to_string(),
                "false",
            )),
            ("&&", _, Node::BoolLiteral(false)) if is_pure_expression(&left.node) => Some((
                "`expr && false` always evaluates to `false`".to_string(),
                "false",
            )),
            _ => None,
        }
    }

    pub(super) fn check_eager_collection_conversion(&mut self, expected: &TypeExpr, value: &SNode) {
        let Some(sink) = Self::expected_collection_sink(expected) else {
            return;
        };
        if !Self::expr_yields_iter(&value.node) {
            return;
        }
        let (kind_word, collection_label) = match sink {
            "to_list" => ("list", "list"),
            "to_set" => ("set", "set"),
            "to_dict" => ("dict", "dict"),
            _ => return,
        };
        let _ = kind_word;
        let message = format!(
            "expression is an iterator; expected {collection_label}. \
             Add .{sink}() to materialize."
        );
        let fix = append_sink_fix(value.span, sink);
        self.diagnostics.push(LintDiagnostic {
            code: Code::LintEagerCollectionConversion,
            rule: "eager-collection-conversion",
            message,
            span: value.span,
            severity: LintSeverity::Warning,
            suggestion: Some(format!("append `.{sink}()` to materialize the iterator")),
            fix: Some(fix),
        });
    }

    pub(super) fn clone_call_receiver(node: &SNode) -> Option<&SNode> {
        match &node.node {
            Node::MethodCall {
                object,
                method,
                args,
            } if method == "clone" && args.is_empty() => Some(object),
            _ => None,
        }
    }

    pub(super) fn check_redundant_clone_args(&mut self, callee: &str, args: &[SNode]) {
        for arg in args {
            let Some(receiver) = Self::clone_call_receiver(arg) else {
                continue;
            };
            let is_drop = callee == "drop";
            let message = if is_drop {
                "cloned value is immediately dropped".to_string()
            } else {
                "cloned value is immediately passed by value".to_string()
            };
            let suggestion = if is_drop {
                "drop the original value directly".to_string()
            } else {
                "pass the original value directly unless a distinct snapshot is required"
                    .to_string()
            };
            let fix = remove_method_call_wrapper_fix(self.source, arg.span, receiver.span);
            self.diagnostics.push(LintDiagnostic {
                code: Code::LintRedundantClone,
                rule: "redundant-clone",
                message,
                span: arg.span,
                severity: LintSeverity::Warning,
                suggestion: Some(suggestion),
                fix,
            });
        }
    }

    pub(super) fn record_param_type_references(&mut self, params: &[TypedParam]) {
        for param in params {
            if let Some(type_expr) = &param.type_expr {
                self.record_type_expr_references(type_expr);
            }
        }
    }

    pub(super) fn has_interpolation(node: &SNode) -> bool {
        use harn_lexer::StringSegment;
        matches!(&node.node, Node::InterpolatedString(segments) if segments.iter().any(|segment| matches!(segment, StringSegment::Expression(_, _, _))))
    }

    /// Returns true if the function is a boundary API that returns untyped/opaque data.
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

    /// Extract the root variable name from an assignment target.
    /// For `x = ...` returns `x`, for `x.foo = ...` or `x[i] = ...` returns `x`.
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
        if name == "secret_scan" {
            return true;
        }
        matches!(
            (name, args.get(1).and_then(Self::string_literal_value)),
            ("mcp_call", Some("harn.secret_scan" | "harn::secret_scan"))
        ) || matches!(
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

    fn string_literal_value(node: &SNode) -> Option<&str> {
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
            rule: "long-running-without-cleanup",
            message: format!(
                "`{name}` starts long-running work without a defer/finally cleanup path"
            ),
            span,
            severity: LintSeverity::Warning,
            suggestion: Some(
                "store the returned handle and cancel it from a defer or finally block with `tools.cancel_handle`"
                    .to_string(),
            ),
            fix: None,
        });
    }

    pub(super) fn call_uses_long_running_flag(name: &str, args: &[SNode]) -> bool {
        if !Self::long_running_capable_call(name, args) {
            return false;
        }
        args.iter().any(Self::expr_has_long_running_flag)
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

    fn expr_has_long_running_flag(node: &SNode) -> bool {
        match &node.node {
            Node::DictLiteral(entries) => entries.iter().any(|entry| {
                matches!(
                    Self::dict_key_name(&entry.key).as_deref(),
                    Some("long_running" | "background")
                ) && matches!(entry.value.node, Node::BoolLiteral(true))
            }),
            _ => false,
        }
    }

    fn body_has_long_running_cleanup(body: &[SNode]) -> bool {
        body.iter().any(Self::node_has_long_running_cleanup)
    }

    fn node_has_long_running_cleanup(node: &SNode) -> bool {
        match &node.node {
            Node::DeferStmt { body } => Self::block_calls_cancel_handle(body),
            Node::TryCatch {
                has_catch: _,
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
        match &node.node {
            Node::FunctionCall { name, args, .. } => {
                name == "cancel_handle"
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
                    || args.iter().any(Self::node_calls_cancel_handle)
            }
            Node::DictLiteral(entries) => entries.iter().any(|entry| {
                Self::node_calls_cancel_handle(&entry.key)
                    || Self::node_calls_cancel_handle(&entry.value)
            }),
            Node::ListLiteral(items) => items.iter().any(Self::node_calls_cancel_handle),
            Node::IfElse {
                condition,
                then_body,
                else_body,
            } => {
                Self::node_calls_cancel_handle(condition)
                    || Self::block_calls_cancel_handle(then_body)
                    || else_body
                        .as_ref()
                        .is_some_and(|body| Self::block_calls_cancel_handle(body))
            }
            Node::TryCatch {
                has_catch: _,
                body,
                catch_body,
                finally_body,
                ..
            } => {
                Self::block_calls_cancel_handle(body)
                    || Self::block_calls_cancel_handle(catch_body)
                    || finally_body
                        .as_ref()
                        .is_some_and(|body| Self::block_calls_cancel_handle(body))
            }
            Node::DeferStmt { body }
            | Node::ForIn { body, .. }
            | Node::WhileLoop { body, .. }
            | Node::Retry { body, .. }
            | Node::CostRoute { body, .. }
            | Node::Block(body)
            | Node::SpawnExpr { body }
            | Node::ScopeBlock { body }
            | Node::Closure { body, .. } => Self::block_calls_cancel_handle(body),
            Node::MethodCall { object, args, .. }
            | Node::OptionalMethodCall { object, args, .. } => {
                Self::node_calls_cancel_handle(object)
                    || args.iter().any(Self::node_calls_cancel_handle)
            }
            Node::PropertyAccess { object, .. }
            | Node::OptionalPropertyAccess { object, .. }
            | Node::SubscriptAccess { object, .. }
            | Node::OptionalSubscriptAccess { object, .. }
            | Node::SliceAccess { object, .. } => Self::node_calls_cancel_handle(object),
            Node::BinaryOp { left, right, .. } => {
                Self::node_calls_cancel_handle(left) || Self::node_calls_cancel_handle(right)
            }
            Node::UnaryOp { operand, .. }
            | Node::TryOperator { operand }
            | Node::TryStar { operand } => Self::node_calls_cancel_handle(operand),
            Node::Ternary {
                condition,
                true_expr,
                false_expr,
            } => {
                Self::node_calls_cancel_handle(condition)
                    || Self::node_calls_cancel_handle(true_expr)
                    || Self::node_calls_cancel_handle(false_expr)
            }
            Node::ReturnStmt { value } | Node::YieldExpr { value } => value
                .as_ref()
                .is_some_and(|value| Self::node_calls_cancel_handle(value)),
            Node::ThrowStmt { value } | Node::EmitExpr { value } => {
                Self::node_calls_cancel_handle(value)
            }
            Node::AttributedDecl { inner, .. } => Self::node_calls_cancel_handle(inner),
            _ => false,
        }
    }

    fn dict_key_name(node: &SNode) -> Option<String> {
        match &node.node {
            Node::Identifier(value)
            | Node::StringLiteral(value)
            | Node::RawStringLiteral(value) => Some(value.clone()),
            _ => None,
        }
    }

    pub(super) fn simple_binding_name(pattern: &BindingPattern) -> Option<&str> {
        match pattern {
            BindingPattern::Identifier(name) => Some(name),
            _ => None,
        }
    }

    pub(super) fn record_mcp_registry_binding(&mut self, name: &str, value: &SNode) {
        let missing = self.mcp_missing_annotation_spans(value);
        if missing.is_empty() {
            self.mcp_registry_missing_annotation_spans.remove(name);
        } else {
            self.mcp_registry_missing_annotation_spans
                .insert(name.to_string(), missing);
        }
    }

    pub(super) fn warn_mcp_tools_missing_annotations(&mut self, value: &SNode) {
        for span in self.mcp_missing_annotation_spans(value) {
            self.warn_missing_mcp_tool_annotations(span);
        }
    }

    fn mcp_missing_annotation_spans(&self, node: &SNode) -> Vec<Span> {
        match &node.node {
            Node::Identifier(name) => self
                .mcp_registry_missing_annotation_spans
                .get(name)
                .cloned()
                .unwrap_or_default(),
            Node::FunctionCall { name, args, .. } if name == "tool_define" => {
                let mut spans = args
                    .first()
                    .map(|arg| self.mcp_missing_annotation_spans(arg))
                    .unwrap_or_default();
                if !Self::tool_define_has_annotations(args) {
                    spans.push(node.span);
                }
                spans
            }
            _ => Vec::new(),
        }
    }

    fn tool_define_has_annotations(args: &[SNode]) -> bool {
        let Some(config) = args.get(3) else {
            return false;
        };
        let Node::DictLiteral(entries) = &config.node else {
            return false;
        };
        entries
            .iter()
            .any(|entry| Self::dict_key_name(&entry.key).as_deref() == Some("annotations"))
    }

    fn warn_missing_mcp_tool_annotations(&mut self, span: Span) {
        self.diagnostics.push(LintDiagnostic {
            code: Code::LintMcpToolAnnotations,
            rule: "mcp-tool-annotations",
            message: "MCP-exposed `tool_define` registration has no `annotations`".to_string(),
            span,
            severity: LintSeverity::Warning,
            suggestion: Some(
                "add MCP `annotations` such as `readOnlyHint`, `destructiveHint`, `idempotentHint`, and `openWorldHint` before passing the registry to `mcp_tools`"
                    .to_string(),
            ),
            fix: None,
        });
    }

    fn warn_missing_secret_scan(&mut self, span: Span) {
        self.diagnostics.push(LintDiagnostic {
            code: Code::LintPrOpenWithoutSecretScan,
            rule: "pr-open-without-secret-scan",
            message: "PR-open flow calls `git::push_pr` without a preceding `secret_scan(...)` in the same handler".to_string(),
            span,
            severity: LintSeverity::Warning,
            suggestion: Some(
                "run `secret_scan(content)` first and gate the PR-open call on an empty findings list"
                    .to_string(),
            ),
            fix: None,
        });
    }

    fn analyze_secret_scan_expr(&mut self, node: &SNode, scanned: bool) -> bool {
        match &node.node {
            Node::FunctionCall { name, args, .. } => {
                let mut state = scanned;
                for arg in args {
                    state = self.analyze_secret_scan_expr(arg, state);
                }
                if Self::is_secret_scan_call(name, args) {
                    return true;
                }
                if Self::is_pr_open_call(name, args) && !state {
                    self.warn_missing_secret_scan(node.span);
                }
                state
            }
            Node::MethodCall { object, args, .. }
            | Node::OptionalMethodCall { object, args, .. } => {
                let mut state = self.analyze_secret_scan_expr(object, scanned);
                for arg in args {
                    state = self.analyze_secret_scan_expr(arg, state);
                }
                state
            }
            Node::PropertyAccess { object, .. }
            | Node::OptionalPropertyAccess { object, .. }
            | Node::Spread(object)
            | Node::TryOperator { operand: object }
            | Node::TryStar { operand: object }
            | Node::UnaryOp {
                operand: object, ..
            } => self.analyze_secret_scan_expr(object, scanned),
            Node::SubscriptAccess { object, index }
            | Node::OptionalSubscriptAccess { object, index } => {
                let state = self.analyze_secret_scan_expr(object, scanned);
                self.analyze_secret_scan_expr(index, state)
            }
            Node::SliceAccess { object, start, end } => {
                let mut state = self.analyze_secret_scan_expr(object, scanned);
                if let Some(start) = start {
                    state = self.analyze_secret_scan_expr(start, state);
                }
                if let Some(end) = end {
                    state = self.analyze_secret_scan_expr(end, state);
                }
                state
            }
            Node::BinaryOp { left, right, .. } => {
                let state = self.analyze_secret_scan_expr(left, scanned);
                self.analyze_secret_scan_expr(right, state)
            }
            Node::Ternary {
                condition,
                true_expr,
                false_expr,
            } => {
                let state = self.analyze_secret_scan_expr(condition, scanned);
                let then_state = self.analyze_secret_scan_expr(true_expr, state);
                let else_state = self.analyze_secret_scan_expr(false_expr, state);
                then_state && else_state
            }
            Node::ListLiteral(items) | Node::OrPattern(items) => {
                items.iter().fold(scanned, |state, item| {
                    self.analyze_secret_scan_expr(item, state)
                })
            }
            Node::DictLiteral(entries)
            | Node::StructConstruct {
                fields: entries, ..
            } => {
                let mut state = scanned;
                for entry in entries {
                    state = self.analyze_secret_scan_expr(&entry.key, state);
                    state = self.analyze_secret_scan_expr(&entry.value, state);
                }
                state
            }
            Node::EnumConstruct { args, .. } => args.iter().fold(scanned, |state, arg| {
                self.analyze_secret_scan_expr(arg, state)
            }),
            Node::Block(body) => self.analyze_secret_scan_block(body, scanned),
            Node::Closure { body, .. } => {
                let _ = self.analyze_secret_scan_block(body, false);
                scanned
            }
            _ => scanned,
        }
    }

    fn analyze_secret_scan_node(&mut self, node: &SNode, scanned: bool) -> bool {
        match &node.node {
            Node::LetBinding { value, .. } | Node::VarBinding { value, .. } => {
                self.analyze_secret_scan_expr(value, scanned)
            }
            Node::Assignment { target, value, .. } => {
                let state = self.analyze_secret_scan_expr(target, scanned);
                self.analyze_secret_scan_expr(value, state)
            }
            Node::IfElse {
                condition,
                then_body,
                else_body,
            } => {
                let state = self.analyze_secret_scan_expr(condition, scanned);
                let then_state = self.analyze_secret_scan_block(then_body, state);
                let Some(else_body) = else_body.as_ref() else {
                    return state;
                };
                let else_state = self.analyze_secret_scan_block(else_body, state);
                then_state && else_state
            }
            Node::ForIn { iterable, body, .. } => {
                let state = self.analyze_secret_scan_expr(iterable, scanned);
                let _ = self.analyze_secret_scan_block(body, state);
                state
            }
            Node::WhileLoop { condition, body } => {
                let state = self.analyze_secret_scan_expr(condition, scanned);
                let _ = self.analyze_secret_scan_block(body, state);
                state
            }
            Node::Retry { count, body } => {
                let state = self.analyze_secret_scan_expr(count, scanned);
                let _ = self.analyze_secret_scan_block(body, state);
                state
            }
            Node::CostRoute { options, body } => {
                let mut state = scanned;
                for (_, value) in options {
                    state = self.analyze_secret_scan_expr(value, state);
                }
                let _ = self.analyze_secret_scan_block(body, state);
                state
            }
            Node::TryCatch {
                has_catch: _,
                body,
                catch_body,
                finally_body,
                ..
            } => {
                let try_state = self.analyze_secret_scan_block(body, scanned);
                let catch_state = self.analyze_secret_scan_block(catch_body, scanned);
                let finally_state = finally_body
                    .as_ref()
                    .map(|body| self.analyze_secret_scan_block(body, scanned))
                    .unwrap_or(scanned);
                if finally_state {
                    true
                } else {
                    try_state && catch_state
                }
            }
            Node::TryExpr { body } => self.analyze_secret_scan_block(body, scanned),
            Node::MatchExpr { value, arms } => {
                let state = self.analyze_secret_scan_expr(value, scanned);
                if arms.is_empty() {
                    return state;
                }
                let mut all_arms_scanned = true;
                for arm in arms {
                    let mut arm_state = self.analyze_secret_scan_expr(&arm.pattern, state);
                    if let Some(guard) = arm.guard.as_ref() {
                        arm_state = self.analyze_secret_scan_expr(guard, arm_state);
                    }
                    all_arms_scanned &= self.analyze_secret_scan_block(&arm.body, arm_state);
                }
                all_arms_scanned
            }
            Node::Parallel { expr, body, .. } => {
                let state = self.analyze_secret_scan_expr(expr, scanned);
                let _ = self.analyze_secret_scan_block(body, false);
                state
            }
            Node::SelectExpr {
                cases,
                timeout,
                default_body,
            } => {
                let mut all_cases_scanned = !cases.is_empty();
                for case in cases {
                    let state = self.analyze_secret_scan_expr(&case.channel, scanned);
                    all_cases_scanned &= self.analyze_secret_scan_block(&case.body, state);
                }
                if let Some((timeout_expr, timeout_body)) = timeout {
                    let state = self.analyze_secret_scan_expr(timeout_expr, scanned);
                    all_cases_scanned &= self.analyze_secret_scan_block(timeout_body, state);
                }
                if let Some(default_body) = default_body {
                    all_cases_scanned &= self.analyze_secret_scan_block(default_body, scanned);
                }
                all_cases_scanned
            }
            Node::ReturnStmt { value } => value
                .as_ref()
                .map(|value| self.analyze_secret_scan_expr(value, scanned))
                .unwrap_or(scanned),
            Node::ThrowStmt { value } => self.analyze_secret_scan_expr(value, scanned),
            _ => self.analyze_secret_scan_expr(node, scanned),
        }
    }

    fn analyze_secret_scan_block(&mut self, nodes: &[SNode], scanned: bool) -> bool {
        let mut state = scanned;
        for node in nodes {
            state = self.analyze_secret_scan_node(node, state);
        }
        state
    }

    /// Extract all variable names from a binding pattern.
    pub(super) fn pattern_names(pattern: &BindingPattern) -> Vec<String> {
        match pattern {
            BindingPattern::Identifier(name) => vec![name.clone()],
            BindingPattern::Dict(fields) => fields
                .iter()
                .map(|f| f.alias.as_deref().unwrap_or(&f.key).to_string())
                .collect(),
            BindingPattern::List(elements) => elements.iter().map(|e| e.name.clone()).collect(),
            BindingPattern::Pair(a, b) => vec![a.clone(), b.clone()],
        }
    }

    /// Declare all variables in a binding pattern.
    pub(super) fn declare_pattern_variables(
        &mut self,
        pattern: &BindingPattern,
        span: Span,
        is_mutable: bool,
    ) {
        let is_simple_ident = matches!(pattern, BindingPattern::Identifier(_));
        for name in Self::pattern_names(pattern) {
            self.declare_variable(&name, span, is_mutable, is_simple_ident);
        }
    }

    /// Declare a variable in the current scope, checking for shadowing.
    pub(super) fn declare_variable(
        &mut self,
        name: &str,
        span: Span,
        is_mutable: bool,
        is_simple_ident: bool,
    ) {
        if name == "_" {
            return;
        }

        if !is_mutable {
            if let Some(scope) = self.scopes.last() {
                if scope.contains(name) {
                    self.diagnostics.push(LintDiagnostic {
                        code: Code::LintShadowVariable,
                        rule: "shadow-variable",
                        message: format!(
                            "cannot redeclare immutable variable `{name}` in the same scope"
                        ),
                        span,
                        severity: LintSeverity::Warning,
                        suggestion: Some(format!(
                            "use `var {name}` for a mutable binding, or choose a different name"
                        )),
                        fix: None,
                    });
                }
            }
        }

        if self.scopes.len() > 1 {
            let outer = &self.scopes[..self.scopes.len() - 1];
            if outer.iter().any(|s| s.contains(name)) {
                self.diagnostics.push(LintDiagnostic {
                    code: Code::LintShadowVariable,
                    rule: "shadow-variable",
                    message: format!("variable `{name}` shadows a variable in an outer scope"),
                    span,
                    severity: LintSeverity::Warning,
                    suggestion: Some(format!("consider renaming to avoid shadowing `{name}`")),
                    fix: None,
                });
            }
        }

        if let Some(scope) = self.scopes.last_mut() {
            scope.insert(name.to_string());
        }

        self.declarations.push(Declaration {
            name: name.to_string(),
            span,
            is_mutable,
            is_simple_ident,
        });
    }

    /// Declare a function/closure parameter in the current scope.
    /// Tracked separately from variables for the `unused-parameter` lint rule.
    pub(super) fn declare_parameter(&mut self, name: &str, span: Span) {
        if name == "_" {
            return;
        }

        if self.scopes.len() > 1 {
            let outer = &self.scopes[..self.scopes.len() - 1];
            if outer.iter().any(|s| s.contains(name)) {
                self.diagnostics.push(LintDiagnostic {
                    code: Code::LintShadowVariable,
                    rule: "shadow-variable",
                    message: format!("variable `{name}` shadows a variable in an outer scope"),
                    span,
                    severity: LintSeverity::Warning,
                    suggestion: Some(format!("consider renaming to avoid shadowing `{name}`")),
                    fix: None,
                });
            }
        }

        if let Some(scope) = self.scopes.last_mut() {
            scope.insert(name.to_string());
        }

        self.param_declarations.push(ParamDeclaration {
            name: name.to_string(),
            span,
        });
    }

    pub(crate) fn lint_program(&mut self, nodes: &[SNode]) {
        self.collect_persona_step_metadata(nodes);
        if let Some(src) = self.source {
            check_legacy_doc_comments(src, nodes, &mut self.diagnostics);
            check_blank_line_between_items(src, nodes, &mut self.diagnostics);
            check_trailing_comma(src, &mut self.diagnostics);
            check_import_order(src, nodes, &mut self.diagnostics);
            check_prefer_optional_shorthand(src, nodes, &mut self.diagnostics);
            check_unnecessary_parentheses(src, nodes, &mut self.diagnostics);
        }
        check_deprecated_llm_options(nodes, &mut self.diagnostics);
        check_reminder_lifecycle_literals(nodes, &mut self.diagnostics);
        check_reminder_provider_count(nodes, &mut self.diagnostics);
        check_reminder_role_hint_capabilities(nodes, &mut self.diagnostics);
        for node in nodes {
            self.lint_node(node);
        }
    }

    fn collect_persona_step_metadata(&mut self, nodes: &[SNode]) {
        for node in nodes {
            let Node::AttributedDecl { attributes, inner } = &node.node else {
                continue;
            };
            let is_step = attributes.iter().any(|attr| attr.name == "step");
            let is_persona = attributes.iter().any(|attr| attr.name == "persona");
            if is_step {
                if let Node::FnDecl { name, .. } = &inner.node {
                    self.step_functions.insert(name.clone());
                    let step_name = attributes
                        .iter()
                        .find(|attr| attr.name == "step")
                        .and_then(|attr| attr.named_arg("name"))
                        .and_then(Self::string_literal_value)
                        .unwrap_or(name)
                        .to_string();
                    self.step_names_by_function.insert(name.clone(), step_name);
                }
            }
            if is_persona {
                if let Node::FnDecl { name, body, .. } = &inner.node {
                    let persona_name = attributes
                        .iter()
                        .find(|attr| attr.name == "persona")
                        .and_then(|attr| attr.named_arg("name"))
                        .and_then(Self::string_literal_value)
                        .unwrap_or(name)
                        .to_string();
                    self.collect_persona_calls(&persona_name, body);
                }
            }
        }
        for (call, _, persona) in &self.persona_body_calls {
            if let Some(step_name) = self.step_names_by_function.get(call) {
                self.persona_steps
                    .entry(persona.clone())
                    .or_default()
                    .insert(step_name.clone());
            }
        }
    }

    fn collect_persona_calls(&mut self, persona_name: &str, body: &[SNode]) {
        for node in body {
            self.collect_persona_calls_node(persona_name, node);
        }
    }

    fn collect_persona_calls_node(&mut self, persona_name: &str, node: &SNode) {
        match &node.node {
            Node::FunctionCall { name, args, .. } => {
                self.persona_body_calls
                    .push((name.clone(), node.span, persona_name.to_string()));
                for arg in args {
                    self.collect_persona_calls_node(persona_name, arg);
                }
            }
            Node::LetBinding { value, .. }
            | Node::VarBinding { value, .. }
            | Node::ReturnStmt { value: Some(value) }
            | Node::YieldExpr { value: Some(value) }
            | Node::EmitExpr { value }
            | Node::ThrowStmt { value }
            | Node::Spread(value)
            | Node::TryOperator { operand: value }
            | Node::TryStar { operand: value }
            | Node::UnaryOp { operand: value, .. } => {
                self.collect_persona_calls_node(persona_name, value);
            }
            Node::IfElse {
                condition,
                then_body,
                else_body,
            } => {
                self.collect_persona_calls_node(persona_name, condition);
                self.collect_persona_calls(persona_name, then_body);
                if let Some(else_body) = else_body {
                    self.collect_persona_calls(persona_name, else_body);
                }
            }
            Node::ForIn { iterable, body, .. } => {
                self.collect_persona_calls_node(persona_name, iterable);
                self.collect_persona_calls(persona_name, body);
            }
            Node::WhileLoop { condition, body } => {
                self.collect_persona_calls_node(persona_name, condition);
                self.collect_persona_calls(persona_name, body);
            }
            Node::Retry { count, body } => {
                self.collect_persona_calls_node(persona_name, count);
                self.collect_persona_calls(persona_name, body);
            }
            Node::CostRoute { options, body } => {
                for (_, value) in options {
                    self.collect_persona_calls_node(persona_name, value);
                }
                self.collect_persona_calls(persona_name, body);
            }
            Node::TryCatch {
                has_catch: _,
                body,
                catch_body,
                finally_body,
                ..
            } => {
                self.collect_persona_calls(persona_name, body);
                self.collect_persona_calls(persona_name, catch_body);
                if let Some(finally_body) = finally_body {
                    self.collect_persona_calls(persona_name, finally_body);
                }
            }
            Node::TryExpr { body }
            | Node::SpawnExpr { body }
            | Node::ScopeBlock { body }
            | Node::DeferStmt { body }
            | Node::MutexBlock { body, .. }
            | Node::Block(body)
            | Node::Closure { body, .. } => self.collect_persona_calls(persona_name, body),
            Node::DeadlineBlock { duration, body } => {
                self.collect_persona_calls_node(persona_name, duration);
                self.collect_persona_calls(persona_name, body);
            }
            Node::GuardStmt {
                condition,
                else_body,
            } => {
                self.collect_persona_calls_node(persona_name, condition);
                self.collect_persona_calls(persona_name, else_body);
            }
            Node::RequireStmt { condition, message } => {
                self.collect_persona_calls_node(persona_name, condition);
                if let Some(message) = message {
                    self.collect_persona_calls_node(persona_name, message);
                }
            }
            Node::Parallel {
                expr,
                body,
                options,
                ..
            } => {
                self.collect_persona_calls_node(persona_name, expr);
                for (_, value) in options {
                    self.collect_persona_calls_node(persona_name, value);
                }
                self.collect_persona_calls(persona_name, body);
            }
            Node::SelectExpr {
                cases,
                timeout,
                default_body,
            } => {
                for case in cases {
                    self.collect_persona_calls_node(persona_name, &case.channel);
                    self.collect_persona_calls(persona_name, &case.body);
                }
                if let Some((duration, body)) = timeout {
                    self.collect_persona_calls_node(persona_name, duration);
                    self.collect_persona_calls(persona_name, body);
                }
                if let Some(body) = default_body {
                    self.collect_persona_calls(persona_name, body);
                }
            }
            Node::MatchExpr { value, arms } => {
                self.collect_persona_calls_node(persona_name, value);
                for arm in arms {
                    self.collect_persona_calls_node(persona_name, &arm.pattern);
                    if let Some(guard) = &arm.guard {
                        self.collect_persona_calls_node(persona_name, guard);
                    }
                    self.collect_persona_calls(persona_name, &arm.body);
                }
            }
            Node::MethodCall { object, args, .. }
            | Node::OptionalMethodCall { object, args, .. } => {
                self.collect_persona_calls_node(persona_name, object);
                for arg in args {
                    self.collect_persona_calls_node(persona_name, arg);
                }
            }
            Node::PropertyAccess { object, .. } | Node::OptionalPropertyAccess { object, .. } => {
                self.collect_persona_calls_node(persona_name, object);
            }
            Node::SubscriptAccess { object, index }
            | Node::OptionalSubscriptAccess { object, index } => {
                self.collect_persona_calls_node(persona_name, object);
                self.collect_persona_calls_node(persona_name, index);
            }
            Node::SliceAccess { object, start, end } => {
                self.collect_persona_calls_node(persona_name, object);
                if let Some(start) = start {
                    self.collect_persona_calls_node(persona_name, start);
                }
                if let Some(end) = end {
                    self.collect_persona_calls_node(persona_name, end);
                }
            }
            Node::BinaryOp { left, right, .. } => {
                self.collect_persona_calls_node(persona_name, left);
                self.collect_persona_calls_node(persona_name, right);
            }
            Node::Ternary {
                condition,
                true_expr,
                false_expr,
            } => {
                self.collect_persona_calls_node(persona_name, condition);
                self.collect_persona_calls_node(persona_name, true_expr);
                self.collect_persona_calls_node(persona_name, false_expr);
            }
            Node::Assignment { target, value, .. } => {
                self.collect_persona_calls_node(persona_name, target);
                self.collect_persona_calls_node(persona_name, value);
            }
            Node::EnumConstruct { args, .. } | Node::ListLiteral(args) | Node::OrPattern(args) => {
                for arg in args {
                    self.collect_persona_calls_node(persona_name, arg);
                }
            }
            Node::StructConstruct { fields, .. } | Node::DictLiteral(fields) => {
                for entry in fields {
                    self.collect_persona_calls_node(persona_name, &entry.key);
                    self.collect_persona_calls_node(persona_name, &entry.value);
                }
            }
            Node::HitlExpr { args, .. } => {
                for arg in args {
                    self.collect_persona_calls_node(persona_name, &arg.value);
                }
            }
            Node::AttributedDecl { inner, .. } => {
                self.collect_persona_calls_node(persona_name, inner);
            }
            _ => {}
        }
    }

    pub(super) fn validate_step_hook_target(&mut self, args: &[SNode], span: Span) {
        let Some(persona_pattern) = args.first().and_then(Self::string_literal_value) else {
            return;
        };
        let Some(step_name) = args.get(1).and_then(Self::string_literal_value) else {
            return;
        };
        let matching_personas: Vec<_> = self
            .persona_steps
            .iter()
            .filter(|(persona, _)| Self::simple_glob_match(persona_pattern, persona))
            .collect();
        if matching_personas.is_empty() {
            self.diagnostics.push(LintDiagnostic {
                code: Code::LintPersonaHookTarget,
                rule: "persona-hook-target",
                message: format!(
                    "`register_step_hook` pattern `{persona_pattern}` does not match a statically declared `@persona`"
                ),
                span,
                severity: LintSeverity::Error,
                suggestion: Some("register hooks against a declared persona name or glob".to_string()),
                fix: None,
            });
            return;
        }
        let missing: Vec<_> = matching_personas
            .into_iter()
            .filter_map(|(persona, steps)| (!steps.contains(step_name)).then_some(persona.clone()))
            .collect();
        if missing.is_empty() {
            return;
        }
        self.diagnostics.push(LintDiagnostic {
            code: Code::LintPersonaHookTarget,
            rule: "persona-hook-target",
            message: format!(
                "`register_step_hook` targets step `{step_name}`, but it is not declared by persona(s): {}",
                missing.join(", ")
            ),
            span,
            severity: LintSeverity::Error,
            suggestion: Some("use a step name declared with `@step(name: ...)` and called by the matching `@persona`".to_string()),
            fix: None,
        });
    }

    fn simple_glob_match(pattern: &str, value: &str) -> bool {
        if pattern == "*" {
            return true;
        }
        if let Some(prefix) = pattern.strip_suffix('*') {
            return value.starts_with(prefix);
        }
        if let Some(suffix) = pattern.strip_prefix('*') {
            return value.ends_with(suffix);
        }
        pattern == value
    }

    /// Lint a block of statements, flagging unreachable code after a
    /// terminator (`return`/`throw`).
    pub(super) fn lint_block(&mut self, nodes: &[SNode]) {
        use harn_parser::stmt_definitely_exits;

        let mut found_terminator = false;

        for (idx, node) in nodes.iter().enumerate() {
            if found_terminator {
                self.diagnostics.push(LintDiagnostic {
                    code: Code::LintDeadCodeAfterReturn,
                    rule: "dead-code-after-return",
                    message: "unreachable code after a terminating statement".to_string(),
                    span: node.span,
                    severity: LintSeverity::Warning,
                    suggestion: Some("remove the unreachable code".to_string()),
                    fix: None,
                });
                // Only report once per block.
                break;
            }

            let final_value_expr = self.value_block_depth > 0 && idx + 1 == nodes.len();
            if !final_value_expr {
                self.check_discarded_approval_result(node);
            }

            if let Some(next) = nodes.get(idx + 1) {
                self.check_let_then_return(node, next);
            }

            self.lint_node(node);

            if stmt_definitely_exits(node) {
                found_terminator = true;
            }
        }
    }

    fn check_let_then_return(&mut self, node: &SNode, next: &SNode) {
        let Node::LetBinding {
            pattern: BindingPattern::Identifier(name),
            type_ann: None,
            value,
        } = &node.node
        else {
            return;
        };
        let Node::ReturnStmt {
            value: Some(returned),
        } = &next.node
        else {
            return;
        };
        let Node::Identifier(returned_name) = &returned.node else {
            return;
        };
        if returned_name != name {
            return;
        }

        let fix = self.source.and_then(|src| {
            let value_text = src.get(value.span.start..value.span.end)?;
            Some(vec![FixEdit {
                span: Span::with_offsets(
                    node.span.start,
                    next.span.end,
                    node.span.line,
                    node.span.column,
                ),
                replacement: format!("return {value_text}"),
            }])
        });
        self.diagnostics.push(LintDiagnostic {
            code: Code::LintLetThenReturn,
            rule: "let-then-return",
            message: format!("binding `{name}` is immediately returned"),
            span: Span::with_offsets(
                node.span.start,
                next.span.end,
                node.span.line,
                node.span.column,
            ),
            severity: LintSeverity::Warning,
            suggestion: Some(format!(
                "return the expression assigned to `{name}` directly"
            )),
            fix,
        });
    }

    fn check_discarded_approval_result(&mut self, node: &SNode) {
        // The approval primitive is recognized in either form: the
        // legacy `request_approval(...)` function-call shape (still
        // valid for back-compat) and the first-class `HitlExpr` form
        // produced by the reserved-keyword parser.
        let name = match &node.node {
            Node::FunctionCall { name, .. } if Self::is_approval_record_builtin(name) => {
                name.as_str()
            }
            Node::HitlExpr {
                kind: harn_parser::HitlKind::RequestApproval,
                ..
            } => "request_approval",
            _ => return,
        };
        self.diagnostics.push(LintDiagnostic {
            code: Code::LintUnhandledApprovalResult,
            rule: "unhandled-approval-result",
            message: format!("approval result from `{name}` is discarded"),
            span: node.span,
            severity: LintSeverity::Warning,
            suggestion: Some(
                "bind the result, inspect its signed approver receipts, or explicitly assign it to `_`"
                    .to_string(),
            ),
            fix: None,
        });
    }

    /// Run post-walk analysis and finalize diagnostics.
    pub(crate) fn finalize(&mut self) {
        for decl in &self.declarations {
            if decl.name.starts_with('_') {
                continue;
            }
            if !self.references.contains(&decl.name) {
                let (code, rule, message, suggestion, fix) = if decl.is_simple_ident {
                    (
                        Code::LintUnusedVariable,
                        "unused-variable",
                        format!("variable `{}` is declared but never used", decl.name),
                        format!("prefix with underscore: `_{}`", decl.name),
                        simple_ident_rename_fix(self.source, decl.span, &decl.name),
                    )
                } else {
                    (
                        Code::LintUnusedPatternBinding,
                        "unused-pattern-binding",
                        format!("pattern binding `{}` is never used", decl.name),
                        "rename the binding with an underscore prefix or remove it from the pattern"
                            .to_string(),
                        None,
                    )
                };
                self.diagnostics.push(LintDiagnostic {
                    code,
                    rule,
                    message,
                    span: decl.span,
                    severity: LintSeverity::Warning,
                    suggestion: Some(suggestion),
                    fix,
                });
            }
        }

        for decl in &self.param_declarations {
            if decl.name.starts_with('_') {
                continue;
            }
            if !self.references.contains(&decl.name) {
                self.diagnostics.push(LintDiagnostic {
                    code: Code::LintUnusedParameter,
                    rule: "unused-parameter",
                    message: format!("parameter `{}` is declared but never used", decl.name),
                    span: decl.span,
                    severity: LintSeverity::Warning,
                    suggestion: Some(format!("prefix with underscore: `_{}`", decl.name)),
                    fix: None,
                });
            }
        }

        for import in &self.imports {
            // `pub import { ... } from "..."` re-exports the listed names as
            // part of this module's public surface. They will not have local
            // references in the file by design, so silence `unused-import`.
            if import.is_pub {
                continue;
            }
            let unused: Vec<&String> = import
                .names
                .iter()
                .filter(|n| !self.references.contains(*n))
                .collect();
            let all_unused = unused.len() == import.names.len();
            for name in &unused {
                let fix = self.source.and_then(|src| {
                    if all_unused {
                        let end = if src.get(import.span.end..import.span.end + 1) == Some("\n") {
                            import.span.end + 1
                        } else {
                            import.span.end
                        };
                        Some(vec![FixEdit {
                            span: Span::with_offsets(
                                import.span.start,
                                end,
                                import.span.line,
                                import.span.column,
                            ),
                            replacement: String::new(),
                        }])
                    } else {
                        // Remove this name from the import list, plus the
                        // adjacent comma/space so the list stays well-formed.
                        let region = src.get(import.span.start..import.span.end)?;
                        let name_pos = region.find(name.as_str())?;
                        let abs_start = import.span.start + name_pos;
                        let abs_end = abs_start + name.len();
                        let after = src.get(abs_end..import.span.end)?;
                        let before = src.get(import.span.start..abs_start)?;
                        let (rm_start, rm_end) = if after.starts_with(',') {
                            let extra = if after.get(1..2) == Some(" ") { 2 } else { 1 };
                            (abs_start, abs_end + extra)
                        } else if before.ends_with(", ") {
                            (abs_start - 2, abs_end)
                        } else if before.ends_with(',') {
                            (abs_start - 1, abs_end)
                        } else {
                            (abs_start, abs_end)
                        };
                        Some(vec![FixEdit {
                            span: Span::with_offsets(
                                rm_start,
                                rm_end,
                                import.span.line,
                                import.span.column,
                            ),
                            replacement: String::new(),
                        }])
                    }
                });
                self.diagnostics.push(LintDiagnostic {
                    code: Code::LintUnusedImport,
                    rule: "unused-import",
                    message: format!("imported name `{name}` is never used"),
                    span: import.span,
                    severity: LintSeverity::Warning,
                    suggestion: Some(format!("remove `{name}` from the import")),
                    fix,
                });
            }
        }

        for decl in &self.declarations {
            if !decl.is_mutable {
                continue;
            }
            if !self.assignments.contains(&decl.name) {
                let fix = self.source.and_then(|src| {
                    let region = src.get(decl.span.start..decl.span.end)?;
                    let var_off = region.find("var")?;
                    let abs = decl.span.start + var_off;
                    Some(vec![FixEdit {
                        span: Span::with_offsets(
                            abs,
                            abs + 3,
                            decl.span.line,
                            decl.span.column + var_off,
                        ),
                        replacement: "let".to_string(),
                    }])
                });
                self.diagnostics.push(LintDiagnostic {
                    code: Code::LintMutableNeverReassigned,
                    rule: "mutable-never-reassigned",
                    message: format!(
                        "variable `{}` is declared as `var` but never reassigned",
                        decl.name
                    ),
                    span: decl.span,
                    severity: LintSeverity::Warning,
                    suggestion: Some("use `let` instead of `var`".to_string()),
                    fix,
                });
            }
        }

        for decl in &self.fn_declarations {
            if decl.is_pub || decl.is_method || decl.name.starts_with('_') {
                continue;
            }
            // `fn main` is the auto-invoked entrypoint (see E4.1): the
            // compiler emits a synthetic call to it, so static call-graph
            // analysis can't see any references. Treat it like `pub`.
            if decl.name == "main" {
                continue;
            }
            if self.externally_imported_names.contains(&decl.name) {
                continue;
            }
            if !self.function_references.contains(&decl.name) {
                self.diagnostics.push(LintDiagnostic {
                    code: Code::LintUnusedFunction,
                    rule: "unused-function",
                    message: format!("function `{}` is declared but never used", decl.name),
                    span: decl.span,
                    severity: LintSeverity::Warning,
                    suggestion: Some(format!(
                        "remove the function or prefix with underscore: `_{}`",
                        decl.name
                    )),
                    fix: None,
                });
            }
        }

        for decl in &self.type_declarations {
            if decl.name.starts_with('_') {
                continue;
            }
            if !self.type_references.contains(&decl.name) {
                self.diagnostics.push(LintDiagnostic {
                    code: Code::LintUnusedType,
                    rule: "unused-type",
                    message: format!(
                        "{} `{}` is declared but never referenced",
                        decl.kind, decl.name
                    ),
                    span: decl.span,
                    severity: LintSeverity::Warning,
                    suggestion: Some(format!(
                        "remove the unused {} or reference `{}` from a signature or constructor",
                        decl.kind, decl.name
                    )),
                    fix: None,
                });
            }
        }

        for (name, span, persona_name) in &self.persona_body_calls {
            if self.builtin_functions.contains(name) {
                continue;
            }
            if self.step_functions.contains(name) {
                continue;
            }
            if self.persona_step_allowlist.contains(name) {
                continue;
            }
            if name.starts_with("__") || name.starts_with("hostlib_") {
                continue;
            }
            self.diagnostics.push(LintDiagnostic {
                code: Code::LintPersonaBodyMustCallSteps,
                rule: "persona-body-must-call-steps",
                message: format!(
                    "`@persona` function `{persona_name}` calls `{name}`, which is not declared `@step`"
                ),
                span: *span,
                severity: LintSeverity::Warning,
                suggestion: Some(format!(
                    "add `@step(name: \"{name}\", ...)` to `{name}` or list it in `[lint].persona_step_allowlist`"
                )),
                fix: None,
            });
        }

        // Variables and parameters may hold closures, so treat them as
        // callable when checking for undefined functions below.
        let all_vars: HashSet<String> = self
            .declarations
            .iter()
            .map(|d| d.name.clone())
            .chain(self.param_declarations.iter().map(|p| p.name.clone()))
            .collect();

        // Wildcard imports hide the real name set unless we can fully
        // resolve them to exports.
        if self.use_module_graph_for_wildcards {
            match &self.module_graph_wildcard_exports {
                Some(names) => {
                    self.known_functions.extend(names.iter().cloned());
                }
                None => {
                    return;
                }
            }
        } else if self.has_wildcard_import {
            return;
        }
        for (name, span) in &self.function_calls {
            if self.known_functions.contains(name) {
                continue;
            }
            if all_vars.contains(name) {
                continue;
            }
            if name.starts_with("__") {
                continue;
            }
            if name.starts_with("hostlib_") {
                continue;
            }
            let suggestion = if let Some(closest) =
                find_closest_match(name, self.known_functions.iter().map(|s| s.as_str()), 2)
            {
                format!("did you mean `{closest}`?")
            } else {
                format!("check the spelling or import `{name}`")
            };
            self.diagnostics.push(LintDiagnostic {
                code: Code::LintUndefinedFunction,
                rule: "undefined-function",
                message: format!("function `{name}` is not defined"),
                span: *span,
                severity: LintSeverity::Warning,
                suggestion: Some(suggestion),
                fix: None,
            });
        }
    }
}
