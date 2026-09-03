//! The stateful linter walk: a [`Linter`] collects diagnostics while
//! traversing the AST, then finalizes post-walk checks
//! (unused/undefined symbols, etc.). The large `lint_node` match lives
//! in the [`walk`] submodule.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use harn_lexer::{FixEdit, Span};
use harn_parser::diagnostic::find_closest_match;
use harn_parser::{BindingPattern, DiagnosticCode as Code, Node, SNode, TypeExpr, TypedParam};

use crate::complexity::cyclomatic_complexity;
use crate::decls::{Declaration, FnDeclaration, ImportInfo, ParamDeclaration, TypeDeclaration};
use crate::diagnostic::{LintDiagnostic, LintSeverity, DEFAULT_COMPLEXITY_THRESHOLD};
use crate::fixes::{
    append_sink_fix, is_pure_expression, remove_method_call_wrapper_fix, simple_ident_discard_fix,
};
use crate::naming::{is_pascal_case, is_snake_case, to_pascal_case, to_snake_case};
use crate::rule::{Rule, RuleCtx};
use harness_facts::HarnessFacts;

mod ambient;
mod connector_effects;
mod discarded_result;
mod execution_safety;
pub(crate) mod harness_facts;
mod parameters;
mod program;
mod spans;
mod type_facts;
mod unused_parameters;
mod walk;

use discarded_result::BlockKind;

/// The linter walks the AST and collects diagnostics.
pub(crate) struct Linter<'a> {
    pub(super) diagnostics: Vec<LintDiagnostic>,
    pub(super) scopes: Vec<HashSet<String>>,
    /// Semantic types for bindings visible at the current lexical position.
    /// Kept parallel to `scopes` so shadowing cannot make a codemod consult a
    /// same-named binding from another scope.
    pub(super) typed_scopes: Vec<HashMap<String, TypeExpr>>,
    /// Type-checker facts indexed by the declaration span used by the AST.
    pub(super) binding_types: HashMap<(usize, usize), TypeExpr>,
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
    pub(super) file_path: Option<PathBuf>,
    /// Whether this source runs as a privileged artifact, mirroring
    /// `harn check --trusted-host-dispatch`. Set from
    /// [`crate::LintOptions::trusted_host_dispatch`].
    pub(super) trusted_host_dispatch: bool,
    /// Whether the package manifest declares this file as a provider's
    /// connector module. Set from
    /// [`crate::LintOptions::connector_runtime_module`].
    pub(super) connector_runtime_module: bool,
    /// Function names imported by other files (cross-module analysis).
    /// Functions in this set are not flagged as unused even if they have
    /// no local references, because another file explicitly imports them.
    pub(crate) externally_imported_names: HashSet<String>,
    /// Track whether the current traversal is inside a test pipeline body.
    pub(super) test_pipeline_depth: usize,
    /// Whether a bare `@test` declaration owns this pipeline's inputs.
    /// Owned inputs may be removed when no caller survives the full walk.
    pub(super) test_pipeline_input_owned: bool,
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
    /// Which receivers in this file are the host `Harness`, and the
    /// spelling-independent call recognizer built on them. Collected in a
    /// prepass so the registry rules can consult the same facts the walk does.
    pub(super) harness_facts: HarnessFacts,
    /// Tracks how many enclosing `@complexity(allow)` attributes are
    /// active. When > 0, the cyclomatic-complexity rule is suppressed
    /// for the contained function.
    pub(super) complexity_suppression_depth: usize,
    /// Threshold above which the cyclomatic-complexity rule fires.
    /// Configurable via `[lint].complexity_threshold` in `harn.toml`.
    pub(crate) complexity_threshold: usize,
    /// Method names declared in `impl` blocks in this file. Suppresses the
    /// discarded-pure-result lint on a same-named user method, which — unlike
    /// the built-in collection methods — may exist for its effects.
    pub(super) impl_method_names: HashSet<String>,
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
    /// `@effects` / `@errors` block. Enabled by callers linting stdlib
    /// sources.
    pub(crate) require_stdlib_metadata: bool,
    /// When set, public `fn` declarations without a `/** */` doc comment
    /// warn (`missing-harndoc`). Opt-in via `[lint] require_docstrings`;
    /// implied by `require_stdlib_metadata`.
    pub(crate) require_docstrings: bool,
    /// Lazily-lexed comment tokens, cached so the `missing-harndoc`
    /// suppression gate (which runs per public item) does not re-tokenize
    /// the whole source each time.
    pub(super) cached_comment_toks: Option<Vec<crate::harndoc::LegacyCommentTok>>,
    /// Pluggable rules driven through the registry. Built-in source- and
    /// AST-structural rules live here; engine-pattern and `.harn` rules
    /// join the same list. The intricately-stateful core checks remain
    /// intrinsic to the walk below.
    pub(super) rules: Vec<Box<dyn Rule>>,
    /// Cached `rules.iter().any(Rule::visits_nodes)` so the per-node walk
    /// skips registry dispatch entirely when no rule opts in.
    pub(super) rules_visit_nodes: bool,
}

impl<'a> Linter<'a> {
    pub(crate) fn new(source: Option<&'a str>) -> Self {
        let rules = crate::rule::builtin_rules();
        let rules_visit_nodes = rules.iter().any(|rule| rule.visits_nodes());
        Self {
            diagnostics: Vec::new(),
            scopes: vec![HashSet::new()],
            typed_scopes: vec![HashMap::new()],
            binding_types: HashMap::new(),
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
            file_path: None,
            trusted_host_dispatch: false,
            connector_runtime_module: false,
            externally_imported_names: HashSet::new(),
            test_pipeline_depth: 0,
            test_pipeline_input_owned: false,
            type_declarations: Vec::new(),
            type_references: HashSet::new(),
            return_type_stack: Vec::new(),
            harness_param_stack: Vec::new(),
            harness_facts: HarnessFacts::default(),
            complexity_suppression_depth: 0,
            complexity_threshold: DEFAULT_COMPLEXITY_THRESHOLD,
            impl_method_names: HashSet::new(),
            connector_effect_export_stack: Vec::new(),
            long_running_cleanup_stack: Vec::new(),
            mcp_registry_missing_annotation_spans: HashMap::new(),
            require_stdlib_metadata: false,
            require_docstrings: false,
            cached_comment_toks: None,
            rules,
            rules_visit_nodes,
        }
    }

    /// Drive every registered rule through one phase `hook`, in
    /// registration order. The rule list is moved out for the duration
    /// so each rule can mutate its own state while still writing into
    /// `self.diagnostics`.
    fn run_rule_phase(
        &mut self,
        mut hook: impl FnMut(&mut dyn Rule, &RuleCtx<'_>, &mut Vec<LintDiagnostic>),
    ) {
        let mut rules = std::mem::take(&mut self.rules);
        let ctx = RuleCtx {
            source: self.source,
            file_path: self.file_path.as_deref(),
            connector_runtime_module: self.connector_runtime_module,
            harness: &self.harness_facts,
        };
        for rule in &mut rules {
            hook(rule.as_mut(), &ctx, &mut self.diagnostics);
        }
        self.rules = rules;
    }

    /// Run every rule's whole-program hook, before the AST walk.
    pub(super) fn run_program_rules(&mut self, program: &[SNode]) {
        self.run_rule_phase(|rule, ctx, out| rule.check_program(program, ctx, out));
    }

    /// Run every rule's per-node hook for `node`. Skipped wholesale
    /// unless at least one rule opted into the walk phase.
    pub(super) fn run_node_rules(&mut self, node: &SNode) {
        self.run_rule_phase(|rule, ctx, out| rule.check_node(node, ctx, out));
    }

    /// Run every rule's post-walk hook.
    fn run_finalize_rules(&mut self) {
        self.run_rule_phase(|rule, ctx, out| rule.finalize(ctx, out));
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

    pub(super) fn in_test_pipeline(&self) -> bool {
        self.test_pipeline_depth > 0
    }

    /// Whether an `assert` here is a test assert.
    ///
    /// [`in_test_pipeline`](Self::in_test_pipeline) is lexical and sees only a
    /// `pipeline test_*` body, so a helper `fn` that a test pipeline calls
    /// reads as production control flow no matter where it lives. Keying the
    /// rule on the enclosing function rather than on the file is what made
    /// every assert in a test helper a finding, which is a false positive by
    /// construction: a file under a test root is a test file in all of it.
    ///
    /// A file linted without a path (stdin, an embedded snippet) keeps the
    /// old behaviour, because there is nothing to read a root from.
    pub(super) fn in_test_source(&self) -> bool {
        self.in_test_pipeline()
            || self
                .file_path
                .as_deref()
                .is_some_and(crate::is_test_source_path)
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

    /// Warn when a public stdlib `pub fn` is missing one or more of the
    /// required metadata fields (`@effects`, `@errors`). Only runs when
    /// callers opted in via
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
                "public stdlib function `{name}` is missing the `@effects`/`@errors` metadata block"
            )
        } else {
            format!(
                "public stdlib function `{name}` is missing required metadata fields: {missing_list}"
            )
        };
        self.diagnostics.push(LintDiagnostic {
            code: Code::LintMissingStdlibMetadata,
            rule: "missing-stdlib-metadata".into(),
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
            rule: "cyclomatic-complexity".into(),
            message: format!(
                "function `{name}` has cyclomatic complexity {complexity} (> {threshold})"
            ),
            span: self.name_anchored_span(name, span),
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
            rule: "naming-convention".into(),
            message: format!("function `{name}` should use snake_case"),
            span: self.name_anchored_span(name, span),
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
            rule: "naming-convention".into(),
            message: format!("{kind} `{name}` should use PascalCase"),
            span: self.name_anchored_span(name, span),
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
            TypeExpr::Union(types) | TypeExpr::Intersection(types) | TypeExpr::Tuple(types) => {
                for inner in types {
                    self.record_type_expr_references(inner);
                }
            }
            TypeExpr::Shape(fields) => {
                for field in fields {
                    self.record_type_expr_references(&field.type_expr);
                }
            }
            TypeExpr::OpenShape { fields, rests } => {
                for field in fields {
                    self.record_type_expr_references(&field.type_expr);
                }
                for rest in rests {
                    self.record_type_expr_references(rest);
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
            rule: "eager-collection-conversion".into(),
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
                rule: "redundant-clone".into(),
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

    pub(super) fn record_callable_signature_type_references(
        &mut self,
        params: &[TypedParam],
        return_type: &Option<TypeExpr>,
    ) {
        self.record_param_type_references(params);
        if let Some(type_expr) = return_type {
            self.record_type_expr_references(type_expr);
        }
    }

    pub(super) fn lint_param_default_values(&mut self, params: &[TypedParam]) {
        for param in params {
            if let Some(default_value) = param.default_value.as_deref() {
                self.lint_node(default_value);
            }
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
            rule: "mcp-tool-annotations".into(),
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
            Node::ValueCall { callee, args } => {
                let mut state = self.analyze_secret_scan_expr(callee, scanned);
                for arg in args {
                    state = self.analyze_secret_scan_expr(arg, state);
                }
                state
            }
            Node::MethodCall {
                object,
                method,
                args,
            }
            | Node::OptionalMethodCall {
                object,
                method,
                args,
            } => {
                let mut state = self.analyze_secret_scan_expr(object, scanned);
                for arg in args {
                    state = self.analyze_secret_scan_expr(arg, state);
                }
                if let Some(state) =
                    self.harness_mcp_secret_scan_state(object, method, args, state, node.span)
                {
                    return state;
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
            Node::LetBinding { value, .. } | Node::ConstBinding { value, .. } => {
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
                ..
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
        is_pub: bool,
    ) {
        let is_simple_ident = matches!(pattern, BindingPattern::Identifier(_));
        let is_externally_reachable = is_pub && self.scopes.len() == 1;
        for name in Self::pattern_names(pattern) {
            self.declare_variable(
                &name,
                span,
                is_mutable,
                is_simple_ident,
                is_externally_reachable,
            );
        }
        if let BindingPattern::Identifier(name) = pattern {
            let key = (span.start, span.end);
            if let Some(type_expr) = self.binding_types.get(&key).cloned() {
                if let Some(scope) = self.typed_scopes.last_mut() {
                    scope.insert(name.clone(), type_expr);
                }
            }
        }
    }

    /// Declare a variable in the current scope, checking for shadowing.
    pub(super) fn declare_variable(
        &mut self,
        name: &str,
        span: Span,
        is_mutable: bool,
        is_simple_ident: bool,
        is_externally_reachable: bool,
    ) {
        if name == "_" {
            return;
        }
        if !is_mutable {
            if let Some(scope) = self.scopes.last() {
                if scope.contains(name) {
                    self.diagnostics.push(LintDiagnostic {
                        code: Code::LintShadowVariable,
                        rule: "shadow-variable".into(),
                        message: format!(
                            "cannot redeclare immutable variable `{name}` in the same scope"
                        ),
                        span,
                        severity: LintSeverity::Warning,
                        suggestion: Some(format!(
                            "use `let {name}` for a mutable binding, or choose a different name"
                        )),
                        fix: None,
                    });
                }
            }
        }
        self.warn_if_shadows_outer_scope(name, span);

        if let Some(scope) = self.scopes.last_mut() {
            scope.insert(name.to_string());
        }

        self.declarations.push(Declaration {
            name: name.to_string(),
            span,
            is_mutable,
            is_simple_ident,
            is_externally_reachable,
        });
    }

    /// Declare a function/closure parameter in the current scope.
    /// Tracked separately from variables for the `unused-parameter` lint rule.
    pub(super) fn declare_parameter(&mut self, name: &str, span: Span) {
        if name == "_" {
            return;
        }
        self.warn_if_shadows_outer_scope(name, span);

        if let Some(scope) = self.scopes.last_mut() {
            scope.insert(name.to_string());
        }

        self.param_declarations.push(ParamDeclaration {
            name: name.to_string(),
            span,
            removable_pipeline: None,
        });
    }

    /// Emit a `shadow-variable` warning when `name` is already bound in any
    /// enclosing (non-current) scope. Shared by variable and parameter
    /// declaration so the two stay in lockstep.
    pub(super) fn warn_if_shadows_outer_scope(&mut self, name: &str, span: Span) {
        // Lifecycle callbacks bind their own root `harness` at execution boundaries.
        // runtime-supplied `harness`, so warning about that nested authority
        // boundary would force authors away from the canonical spelling.
        // Same-scope redeclarations are still diagnosed above.
        if name == "harness" {
            return;
        }
        if self.scopes.len() <= 1 {
            return;
        }
        let outer = &self.scopes[..self.scopes.len() - 1];
        if outer.iter().any(|s| s.contains(name)) {
            self.diagnostics.push(LintDiagnostic {
                code: Code::LintShadowVariable,
                rule: "shadow-variable".into(),
                message: format!("variable `{name}` shadows a variable in an outer scope"),
                span,
                severity: LintSeverity::Warning,
                suggestion: Some(format!("consider renaming to avoid shadowing `{name}`")),
                fix: None,
            });
        }
    }

    /// Lint a statement block whose trailing expression is discarded — a `fn`
    /// / `for` / `while` / `try` body. Harn has no implicit block return, so
    /// every node here is in statement position, the tail included.
    pub(super) fn lint_block(&mut self, nodes: &[SNode]) {
        self.lint_block_of_kind(nodes, BlockKind::Statement);
    }

    /// Lint a block whose trailing expression *is* the block's value — a
    /// closure body, `match` arm, `if`/`else` branch, or `block { … }`. The
    /// tail is a result, not a discarded statement, so result-discarding
    /// checks must skip it.
    pub(super) fn lint_value_block(&mut self, nodes: &[SNode]) {
        self.lint_block_of_kind(nodes, BlockKind::Value);
    }

    /// Lint a block of statements, flagging unreachable code after a
    /// terminator (`return`/`throw`).
    ///
    /// `kind` governs the tail node only, and describes the *immediately*
    /// enclosing block — it deliberately does not nest. A `for` body inside a
    /// closure is still a statement block, so its tail is still discarded.
    fn lint_block_of_kind(&mut self, nodes: &[SNode], kind: BlockKind) {
        use harn_parser::stmt_definitely_exits;

        let mut found_terminator = false;

        for (idx, node) in nodes.iter().enumerate() {
            if found_terminator {
                self.diagnostics.push(LintDiagnostic {
                    code: Code::LintDeadCodeAfterReturn,
                    rule: "dead-code-after-return".into(),
                    message: "unreachable code after a terminating statement".to_string(),
                    span: node.span,
                    severity: LintSeverity::Warning,
                    suggestion: Some("remove the unreachable code".to_string()),
                    fix: None,
                });
                // Only report once per block.
                break;
            }

            let final_value_expr = kind == BlockKind::Value && idx + 1 == nodes.len();
            if !final_value_expr {
                self.check_discarded_approval_result(node);
                self.check_discarded_pure_result(node);
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
        // Applies to either binding form — the simplification `X x = expr;
        // return x` → `return expr` is valid whether `x` is a mutable `let`
        // or an immutable `const`.
        let (Node::LetBinding {
            pattern: BindingPattern::Identifier(name),
            type_ann: None,
            value,
            ..
        }
        | Node::ConstBinding {
            pattern: BindingPattern::Identifier(name),
            type_ann: None,
            value,
            ..
        }) = &node.node
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
            rule: "let-then-return".into(),
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

    /// Run post-walk analysis and finalize diagnostics. The intrinsic
    /// core checks (unused/undefined symbols) run first, then every
    /// registered rule's finalize hook, regardless of the core's
    /// early exits.
    pub(crate) fn finalize(&mut self) {
        self.finalize_core();
        self.run_finalize_rules();
    }

    fn finalize_core(&mut self) {
        for decl in &self.declarations {
            if decl.is_externally_reachable {
                continue;
            }
            if !decl.is_simple_ident && decl.name.starts_with('_') {
                continue;
            }
            if !self.references.contains(&decl.name) {
                let (code, rule, message, suggestion, fix) = if decl.is_simple_ident {
                    (
                        Code::LintUnusedVariable,
                        "unused-variable",
                        format!("variable `{}` is declared but never used", decl.name),
                        "replace with discard binding: `_`".to_string(),
                        simple_ident_discard_fix(self.source, decl.span, &decl.name),
                    )
                } else {
                    (
                        Code::LintUnusedPatternBinding,
                        "unused-pattern-binding",
                        format!("pattern binding `{}` is never used", decl.name),
                        "rename the binding to `_`, prefix it with `_` if the name carries useful intent, or remove it from the pattern"
                            .to_string(),
                        None,
                    )
                };
                self.diagnostics.push(LintDiagnostic {
                    code,
                    rule: rule.into(),
                    message,
                    span: decl.span,
                    severity: LintSeverity::Warning,
                    suggestion: Some(suggestion),
                    fix,
                });
            }
        }

        self.finalize_unused_parameters();

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
                // A name used only in type position (`import { T }` consumed
                // by annotations of a `pub type` / struct / enum alias) is
                // still a real use.
                .filter(|name| import.is_unused(name, &self.references, &self.type_references))
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
                    rule: "unused-import".into(),
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
            // Only tighten simple `let x = …` bindings. A destructuring `let`
            // governs every name it binds, so it can only become `const` when
            // *all* of those names are never reassigned; tightening because a
            // single field is never reassigned would freeze a sibling that IS
            // reassigned (HARN-OWN-001, immutable assignment). Skip
            // destructuring here — a sound all-fields check can come later.
            if !decl.is_simple_ident {
                continue;
            }
            if !self.assignments.contains(&decl.name) {
                let fix = self.source.and_then(|src| {
                    let region = src.get(decl.span.start..decl.span.end)?;
                    let kw_off = region.find("let")?;
                    let abs = decl.span.start + kw_off;
                    Some(vec![FixEdit {
                        span: Span::with_offsets(
                            abs,
                            abs + 3,
                            decl.span.line,
                            decl.span.column + kw_off,
                        ),
                        replacement: "const".to_string(),
                    }])
                });
                self.diagnostics.push(LintDiagnostic {
                    code: Code::LintMutableNeverReassigned,
                    rule: "mutable-never-reassigned".into(),
                    message: format!(
                        "variable `{}` is declared as `let` but never reassigned",
                        decl.name
                    ),
                    span: decl.span,
                    severity: LintSeverity::Warning,
                    suggestion: Some("use `const` instead of `let`".to_string()),
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
                    rule: "unused-function".into(),
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
            if decl.is_pub || decl.name.starts_with('_') {
                continue;
            }
            if !self.type_references.contains(&decl.name) {
                self.diagnostics.push(LintDiagnostic {
                    code: Code::LintUnusedType,
                    rule: "unused-type".into(),
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
            if self.builtin_functions.contains(name) || harn_parser::is_legacy_ambient_builtin(name)
            {
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
                rule: "persona-body-must-call-steps".into(),
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
            if self.known_functions.contains(name) || harn_parser::is_legacy_ambient_builtin(name) {
                continue;
            }
            if all_vars.contains(name) {
                continue;
            }
            if name.starts_with("__") || name.starts_with("hostlib_") {
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
                rule: "undefined-function".into(),
                message: format!("function `{name}` is not defined"),
                span: *span,
                severity: LintSeverity::Warning,
                suggestion: Some(suggestion),
                fix: None,
            });
        }
    }
}
