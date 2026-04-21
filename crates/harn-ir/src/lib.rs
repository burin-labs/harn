use std::collections::{BTreeMap, BTreeSet, HashSet, VecDeque};

use harn_lexer::Span;
use harn_parser::{Attribute, AttributeArg, BindingPattern, Node, SNode};

pub type NodeId = usize;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HandlerKind {
    Function,
    Tool,
    Pipeline,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvariantSpec {
    pub name: String,
    pub span: Span,
    pub params: BTreeMap<String, String>,
    pub positionals: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct HandlerSpec {
    pub name: String,
    pub kind: HandlerKind,
    pub span: Span,
    pub body: Vec<SNode>,
    pub invariants: Vec<InvariantSpec>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathStep {
    pub span: Span,
    pub label: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvariantDiagnostic {
    pub invariant: String,
    pub handler: String,
    pub message: String,
    pub span: Span,
    pub help: Option<String>,
    pub path: Vec<PathStep>,
}

#[derive(Debug, Clone)]
pub struct AnalysisReport {
    pub handlers: Vec<HandlerIr>,
    pub diagnostics: Vec<InvariantDiagnostic>,
}

impl AnalysisReport {
    pub fn handler(&self, name: &str) -> Option<&HandlerIr> {
        self.handlers.iter().find(|handler| handler.name == name)
    }
}

#[derive(Debug, Clone)]
pub struct HandlerIr {
    pub name: String,
    pub kind: HandlerKind,
    pub span: Span,
    pub invariants: Vec<InvariantSpec>,
    pub entry: NodeId,
    pub exit: NodeId,
    pub nodes: Vec<IrNode>,
    pub edges: Vec<IrEdge>,
}

impl HandlerIr {
    pub fn node(&self, id: NodeId) -> &IrNode {
        &self.nodes[id]
    }

    pub fn successors(&self, id: NodeId) -> impl Iterator<Item = NodeId> + '_ {
        self.edges
            .iter()
            .filter(move |edge| edge.from == id)
            .map(|edge| edge.to)
    }
}

#[derive(Debug, Clone)]
pub struct IrEdge {
    pub from: NodeId,
    pub to: NodeId,
}

#[derive(Debug, Clone)]
pub struct IrNode {
    pub id: NodeId,
    pub span: Span,
    pub label: String,
    pub semantics: NodeSemantics,
}

#[derive(Debug, Clone)]
pub enum NodeSemantics {
    Start,
    Exit,
    Marker,
    Branch,
    Call(CallSemantics),
    Assignment(AssignmentSemantics),
    ApprovalScopeEnter,
    ApprovalScopeExit,
    Return,
    Throw,
}

#[derive(Debug, Clone)]
pub struct AssignmentSemantics {
    pub target: Option<String>,
    pub op: Option<String>,
    pub value: ExprSummary,
}

#[derive(Debug, Clone)]
pub enum ExprSummary {
    Reference(String),
    Call(String),
    Binary {
        op: String,
        left: Box<ExprSummary>,
        right: Box<ExprSummary>,
    },
    Literal,
    Unknown,
}

#[derive(Debug, Clone)]
pub struct CallSemantics {
    pub name: String,
    pub display_name: String,
    pub classification: CallClassification,
    pub literal_args: Vec<LiteralValue>,
}

#[derive(Debug, Clone)]
pub enum CallClassification {
    Other,
    ApprovalGate,
    BudgetRead,
    FsWrite { path: Option<String> },
    SideEffect,
}

#[derive(Debug, Clone)]
pub enum LiteralValue {
    String(String),
    Number(String),
    Bool(bool),
    Nil,
    Identifier(String),
    Dict(BTreeMap<String, LiteralValue>),
    List(Vec<LiteralValue>),
    Unknown,
}

impl LiteralValue {
    fn as_str(&self) -> Option<&str> {
        match self {
            Self::String(value) | Self::Identifier(value) => Some(value.as_str()),
            _ => None,
        }
    }

    fn dict_field(&self, key: &str) -> Option<&LiteralValue> {
        match self {
            Self::Dict(entries) => entries.get(key),
            _ => None,
        }
    }
}

pub trait Invariant {
    fn name(&self) -> &'static str;
    fn check(&self, ir: &HandlerIr) -> Vec<InvariantDiagnostic>;
}

#[derive(Debug, Clone)]
pub struct FsWritesSubsetPathGlob {
    globs: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct BudgetRemainingNonIncreasing {
    target: String,
}

#[derive(Debug, Clone, Default)]
pub struct ApprovalReachability;

impl Invariant for FsWritesSubsetPathGlob {
    fn name(&self) -> &'static str {
        "fs.writes"
    }

    fn check(&self, ir: &HandlerIr) -> Vec<InvariantDiagnostic> {
        let mut diagnostics = Vec::new();
        let mut seen = BTreeSet::new();
        for node in &ir.nodes {
            let NodeSemantics::Call(call) = &node.semantics else {
                continue;
            };
            let CallClassification::FsWrite { path } = &call.classification else {
                continue;
            };

            let message = match path.as_deref() {
                Some(path) if self.globs.iter().any(|glob| glob_match(glob, path)) => continue,
                Some(path) => format!(
                    "write path `{path}` is outside the allowed glob(s): {}",
                    self.globs.join(", ")
                ),
                None => format!(
                    "could not prove `{}` stays within the allowed glob(s): {}",
                    call.display_name,
                    self.globs.join(", ")
                ),
            };

            if !seen.insert(node.id) {
                continue;
            }

            diagnostics.push(InvariantDiagnostic {
                invariant: self.name().to_string(),
                handler: ir.name.clone(),
                message,
                span: node.span,
                help: Some(
                    "use a literal path that matches the declared glob, or narrow the dynamic path before writing".to_string(),
                ),
                path: path_to_node(ir, node.id),
            });
        }
        diagnostics
    }
}

impl Invariant for BudgetRemainingNonIncreasing {
    fn name(&self) -> &'static str {
        "budget.remaining"
    }

    fn check(&self, ir: &HandlerIr) -> Vec<InvariantDiagnostic> {
        let mut diagnostics = Vec::new();
        let mut seen = BTreeSet::new();
        for node in &ir.nodes {
            let NodeSemantics::Assignment(assignment) = &node.semantics else {
                continue;
            };
            if assignment.target.as_deref() != Some(self.target.as_str()) {
                continue;
            }
            if assignment_is_non_increasing(assignment, &self.target) {
                continue;
            }
            if !seen.insert(node.id) {
                continue;
            }
            diagnostics.push(InvariantDiagnostic {
                invariant: self.name().to_string(),
                handler: ir.name.clone(),
                message: format!(
                    "assignment to `{}` may increase it; only self-subtractions, identity assignments, or `llm_budget_remaining()` refreshes are accepted",
                    self.target
                ),
                span: node.span,
                help: Some(
                    "rewrite the update as `target = target - delta`, `target -= delta`, or refresh it from `llm_budget_remaining()`".to_string(),
                ),
                path: path_to_node(ir, node.id),
            });
        }
        diagnostics
    }
}

impl Invariant for ApprovalReachability {
    fn name(&self) -> &'static str {
        "approval.reachability"
    }

    fn check(&self, ir: &HandlerIr) -> Vec<InvariantDiagnostic> {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        struct State {
            explicit_approval: bool,
            scoped_approval_depth: u8,
        }

        impl State {
            fn is_approved(self) -> bool {
                self.explicit_approval || self.scoped_approval_depth > 0
            }
        }

        let mut diagnostics = Vec::new();
        let mut queue = VecDeque::new();
        let mut visited = HashSet::new();
        let mut reported = BTreeSet::new();

        queue.push_back((
            ir.entry,
            State {
                explicit_approval: false,
                scoped_approval_depth: 0,
            },
            vec![PathStep {
                span: ir.node(ir.entry).span,
                label: ir.node(ir.entry).label.clone(),
            }],
        ));

        while let Some((node_id, state, path)) = queue.pop_front() {
            if !visited.insert((node_id, state)) {
                continue;
            }

            let node = ir.node(node_id);
            let mut next_state = state;
            match &node.semantics {
                NodeSemantics::Call(call) => match call.classification {
                    CallClassification::ApprovalGate => {
                        next_state.explicit_approval = true;
                    }
                    CallClassification::FsWrite { .. } | CallClassification::SideEffect => {
                        if !state.is_approved() && reported.insert(node_id) {
                            diagnostics.push(InvariantDiagnostic {
                                invariant: self.name().to_string(),
                                handler: ir.name.clone(),
                                message: format!(
                                    "side-effecting call `{}` is reachable before any approval gate",
                                    call.display_name
                                ),
                                span: node.span,
                                help: Some(
                                    "call `request_approval(...)` earlier on every path, or move the side effect into a `dual_control(...)` closure".to_string(),
                                ),
                                path: path.clone(),
                            });
                        }
                    }
                    _ => {}
                },
                NodeSemantics::ApprovalScopeEnter => {
                    next_state.scoped_approval_depth =
                        next_state.scoped_approval_depth.saturating_add(1);
                }
                NodeSemantics::ApprovalScopeExit => {
                    next_state.scoped_approval_depth =
                        next_state.scoped_approval_depth.saturating_sub(1);
                }
                _ => {}
            }

            for succ in ir.successors(node_id) {
                let succ_node = ir.node(succ);
                let mut next_path = path.clone();
                next_path.push(PathStep {
                    span: succ_node.span,
                    label: succ_node.label.clone(),
                });
                queue.push_back((succ, next_state, next_path));
            }
        }

        diagnostics
    }
}

pub fn analyze_program(program: &[SNode]) -> AnalysisReport {
    let (handlers, mut diagnostics) = collect_handlers(program);
    let mut irs = Vec::with_capacity(handlers.len());

    for handler in handlers {
        let ir = HandlerIrBuilder::new(&handler).build();
        for spec in &handler.invariants {
            match instantiate_invariant(spec) {
                Ok(invariant) => diagnostics.extend(invariant.check(&ir)),
                Err(diag) => diagnostics.push(diag.with_handler(&handler.name)),
            }
        }
        irs.push(ir);
    }

    AnalysisReport {
        handlers: irs,
        diagnostics,
    }
}

pub fn explain_handler_invariant(
    program: &[SNode],
    handler_name: &str,
    invariant_name: &str,
) -> Result<Vec<InvariantDiagnostic>, String> {
    let (handlers, config_diags) = collect_handlers(program);
    let Some(handler) = handlers.iter().find(|handler| handler.name == handler_name) else {
        return Err(format!("handler `{handler_name}` was not found"));
    };
    if let Some(diag) = config_diags
        .into_iter()
        .find(|diag| diag.handler == handler.name || diag.handler.is_empty())
    {
        return Ok(vec![diag]);
    }
    let normalized = normalize_invariant_name(invariant_name)
        .ok_or_else(|| format!("unknown invariant `{invariant_name}`"))?;
    let Some(spec) = handler
        .invariants
        .iter()
        .find(|spec| spec.name == normalized)
        .cloned()
    else {
        return Err(format!(
            "handler `{handler_name}` does not declare `@invariant(\"{normalized}\")`"
        ));
    };
    let invariant = instantiate_invariant(&spec).map_err(|diag| diag.message)?;
    let ir = HandlerIrBuilder::new(handler).build();
    Ok(invariant.check(&ir))
}

fn collect_handlers(program: &[SNode]) -> (Vec<HandlerSpec>, Vec<InvariantDiagnostic>) {
    let mut handlers = Vec::new();
    let mut diagnostics = Vec::new();

    for node in program {
        let (attributes, inner) = match &node.node {
            Node::AttributedDecl { attributes, inner } => (attributes.as_slice(), inner.as_ref()),
            _ => (&[][..], node),
        };
        let Some((name, kind, body)) = handler_decl(inner) else {
            continue;
        };
        let (invariants, mut invariant_diags) = parse_invariant_specs(attributes, name, kind);
        diagnostics.append(&mut invariant_diags);
        handlers.push(HandlerSpec {
            name: name.to_string(),
            kind,
            span: inner.span,
            body: body.to_vec(),
            invariants,
        });
    }

    (handlers, diagnostics)
}

fn handler_decl(node: &SNode) -> Option<(&str, HandlerKind, &[SNode])> {
    match &node.node {
        Node::FnDecl { name, body, .. } => Some((name.as_str(), HandlerKind::Function, body)),
        Node::ToolDecl { name, body, .. } => Some((name.as_str(), HandlerKind::Tool, body)),
        Node::Pipeline { name, body, .. } => Some((name.as_str(), HandlerKind::Pipeline, body)),
        _ => None,
    }
}

fn parse_invariant_specs(
    attributes: &[Attribute],
    handler_name: &str,
    handler_kind: HandlerKind,
) -> (Vec<InvariantSpec>, Vec<InvariantDiagnostic>) {
    let mut specs = Vec::new();
    let mut diagnostics = Vec::new();

    for attribute in attributes {
        if attribute.name != "invariant" {
            continue;
        }
        if !matches!(
            handler_kind,
            HandlerKind::Function | HandlerKind::Tool | HandlerKind::Pipeline
        ) {
            diagnostics.push(InvariantDiagnostic {
                invariant: "invariant".to_string(),
                handler: handler_name.to_string(),
                message: "`@invariant` only applies to function, tool, or pipeline declarations"
                    .to_string(),
                span: attribute.span,
                help: None,
                path: Vec::new(),
            });
            continue;
        }

        match parse_invariant_spec(attribute) {
            Ok(spec) => specs.push(spec),
            Err(mut diag) => {
                diag.handler = handler_name.to_string();
                diagnostics.push(diag);
            }
        }
    }

    (specs, diagnostics)
}

fn parse_invariant_spec(attribute: &Attribute) -> Result<InvariantSpec, InvariantDiagnostic> {
    let mut named = BTreeMap::new();
    let mut positionals = Vec::new();

    for arg in &attribute.args {
        let Some(value) = attribute_arg_string(arg) else {
            return Err(InvariantDiagnostic {
                invariant: "invariant".to_string(),
                handler: String::new(),
                message: "`@invariant(...)` arguments must be strings, identifiers, numbers, bools, or nil".to_string(),
                span: arg.span,
                help: Some("use strings for invariant names and configuration values".to_string()),
                path: Vec::new(),
            });
        };
        if let Some(name) = &arg.name {
            named.insert(name.clone(), value);
        } else {
            positionals.push(value);
        }
    }

    let raw_name = named
        .remove("name")
        .or_else(|| positionals.first().cloned())
        .ok_or_else(|| InvariantDiagnostic {
            invariant: "invariant".to_string(),
            handler: String::new(),
            message: "`@invariant(...)` requires an invariant name as the first positional argument or `name:`".to_string(),
            span: attribute.span,
            help: Some(
                "for example: `@invariant(\"fs.writes\", \"src/**\")`".to_string(),
            ),
            path: Vec::new(),
        })?;
    let name = normalize_invariant_name(&raw_name).ok_or_else(|| InvariantDiagnostic {
        invariant: raw_name.clone(),
        handler: String::new(),
        message: format!("unknown invariant `{raw_name}`"),
        span: attribute.span,
        help: Some(
            "known invariants are `fs.writes`, `budget.remaining`, and `approval.reachability`"
                .to_string(),
        ),
        path: Vec::new(),
    })?;

    let remaining_positionals = if named.contains_key("name") {
        positionals
    } else {
        positionals.into_iter().skip(1).collect()
    };

    Ok(InvariantSpec {
        name,
        span: attribute.span,
        params: named,
        positionals: remaining_positionals,
    })
}

fn attribute_arg_string(arg: &AttributeArg) -> Option<String> {
    match &arg.value.node {
        Node::StringLiteral(value) | Node::RawStringLiteral(value) | Node::Identifier(value) => {
            Some(value.clone())
        }
        Node::IntLiteral(value) => Some(value.to_string()),
        Node::FloatLiteral(value) => Some(value.to_string()),
        Node::BoolLiteral(value) => Some(value.to_string()),
        Node::NilLiteral => Some("nil".to_string()),
        _ => None,
    }
}

fn normalize_invariant_name(name: &str) -> Option<String> {
    match name {
        "fs.writes" | "fs_writes" | "writes" => Some("fs.writes".to_string()),
        "budget.remaining" | "budget_remaining" | "budget" => Some("budget.remaining".to_string()),
        "approval.reachability" | "approval_reachability" | "approval" => {
            Some("approval.reachability".to_string())
        }
        _ => None,
    }
}

fn instantiate_invariant(
    spec: &InvariantSpec,
) -> Result<Box<dyn Invariant>, ConfigDiagnosticBuilder> {
    match spec.name.as_str() {
        "fs.writes" => {
            let mut globs = spec.positionals.clone();
            if let Some(glob) = spec
                .params
                .get("path_glob")
                .or_else(|| spec.params.get("glob"))
                .or_else(|| spec.params.get("allow"))
            {
                globs.push(glob.clone());
            }
            if globs.is_empty() {
                return Err(ConfigDiagnosticBuilder::new(
                    "fs.writes",
                    spec.span,
                    "`fs.writes` requires at least one allowed path glob".to_string(),
                    Some("for example: `@invariant(\"fs.writes\", \"src/**\")`".to_string()),
                ));
            }
            Ok(Box::new(FsWritesSubsetPathGlob { globs }))
        }
        "budget.remaining" => {
            let target = spec
                .params
                .get("target")
                .cloned()
                .or_else(|| spec.positionals.first().cloned())
                .unwrap_or_else(|| "budget.remaining".to_string());
            Ok(Box::new(BudgetRemainingNonIncreasing { target }))
        }
        "approval.reachability" => Ok(Box::new(ApprovalReachability)),
        other => Err(ConfigDiagnosticBuilder::new(
            other,
            spec.span,
            format!("unknown invariant `{other}`"),
            None,
        )),
    }
}

#[derive(Debug, Clone)]
struct ConfigDiagnosticBuilder {
    invariant: String,
    span: Span,
    message: String,
    help: Option<String>,
}

impl ConfigDiagnosticBuilder {
    fn new(
        invariant: impl Into<String>,
        span: Span,
        message: String,
        help: Option<String>,
    ) -> Self {
        Self {
            invariant: invariant.into(),
            span,
            message,
            help,
        }
    }

    fn with_handler(self, handler: &str) -> InvariantDiagnostic {
        InvariantDiagnostic {
            invariant: self.invariant,
            handler: handler.to_string(),
            message: self.message,
            span: self.span,
            help: self.help,
            path: Vec::new(),
        }
    }
}

struct HandlerIrBuilder<'a> {
    handler: &'a HandlerSpec,
    nodes: Vec<IrNode>,
    edges: Vec<IrEdge>,
}

impl<'a> HandlerIrBuilder<'a> {
    fn new(handler: &'a HandlerSpec) -> Self {
        Self {
            handler,
            nodes: Vec::new(),
            edges: Vec::new(),
        }
    }

    fn build(mut self) -> HandlerIr {
        let entry = self.push_node(
            self.handler.span,
            "enter handler".to_string(),
            NodeSemantics::Start,
        );
        let exit = self.push_node(
            self.handler.span,
            "exit handler".to_string(),
            NodeSemantics::Exit,
        );
        let exits = self.build_block(&self.handler.body, vec![entry]);
        self.connect_all(&exits, exit);
        HandlerIr {
            name: self.handler.name.clone(),
            kind: self.handler.kind,
            span: self.handler.span,
            invariants: self.handler.invariants.clone(),
            entry,
            exit,
            nodes: self.nodes,
            edges: self.edges,
        }
    }

    fn push_node(&mut self, span: Span, label: String, semantics: NodeSemantics) -> NodeId {
        let id = self.nodes.len();
        self.nodes.push(IrNode {
            id,
            span,
            label,
            semantics,
        });
        id
    }

    fn connect(&mut self, from: NodeId, to: NodeId) {
        self.edges.push(IrEdge { from, to });
    }

    fn connect_all(&mut self, from: &[NodeId], to: NodeId) {
        for &edge_from in from {
            self.connect(edge_from, to);
        }
    }

    fn build_block(&mut self, nodes: &[SNode], incoming: Vec<NodeId>) -> Vec<NodeId> {
        let mut exits = incoming;
        for node in nodes {
            exits = self.build_stmt(node, exits);
        }
        exits
    }

    fn build_stmt(&mut self, node: &SNode, incoming: Vec<NodeId>) -> Vec<NodeId> {
        match &node.node {
            Node::LetBinding { pattern, value, .. } | Node::VarBinding { pattern, value, .. } => {
                let exits = self.build_expr(value, incoming);
                if let BindingPattern::Identifier(name) = pattern {
                    let assignment = self.push_node(
                        node.span,
                        format!("assign {name}"),
                        NodeSemantics::Assignment(AssignmentSemantics {
                            target: Some(name.clone()),
                            op: None,
                            value: expr_summary(value),
                        }),
                    );
                    self.connect_all(&exits, assignment);
                    vec![assignment]
                } else {
                    exits
                }
            }
            Node::Assignment { target, value, op } => {
                let exits = self.build_expr(value, incoming);
                let assignment = self.push_node(
                    node.span,
                    format!(
                        "assign {}",
                        target_path(target).unwrap_or_else(|| "target".to_string())
                    ),
                    NodeSemantics::Assignment(AssignmentSemantics {
                        target: target_path(target),
                        op: op.clone(),
                        value: expr_summary(value),
                    }),
                );
                self.connect_all(&exits, assignment);
                vec![assignment]
            }
            Node::IfElse {
                condition,
                then_body,
                else_body,
            } => {
                let cond_exits = self.build_expr(condition, incoming);
                let branch =
                    self.push_node(node.span, "if condition".to_string(), NodeSemantics::Branch);
                self.connect_all(&cond_exits, branch);

                let then_entry =
                    self.push_node(node.span, "if true".to_string(), NodeSemantics::Marker);
                self.connect(branch, then_entry);
                let mut exits = self.build_block(then_body, vec![then_entry]);

                if let Some(else_body) = else_body {
                    let else_entry =
                        self.push_node(node.span, "if false".to_string(), NodeSemantics::Marker);
                    self.connect(branch, else_entry);
                    exits.extend(self.build_block(else_body, vec![else_entry]));
                } else {
                    let fallthrough =
                        self.push_node(node.span, "if false".to_string(), NodeSemantics::Marker);
                    self.connect(branch, fallthrough);
                    exits.push(fallthrough);
                }

                exits
            }
            Node::GuardStmt {
                condition,
                else_body,
            } => {
                let cond_exits = self.build_expr(condition, incoming);
                let branch = self.push_node(
                    node.span,
                    "guard condition".to_string(),
                    NodeSemantics::Branch,
                );
                self.connect_all(&cond_exits, branch);

                let success =
                    self.push_node(node.span, "guard passed".to_string(), NodeSemantics::Marker);
                self.connect(branch, success);

                let else_entry =
                    self.push_node(node.span, "guard failed".to_string(), NodeSemantics::Marker);
                self.connect(branch, else_entry);

                let mut exits = vec![success];
                exits.extend(self.build_block(else_body, vec![else_entry]));
                exits
            }
            Node::ForIn { iterable, body, .. } => {
                let iter_exits = self.build_expr(iterable, incoming);
                let branch = self.push_node(
                    node.span,
                    "for-in iteration".to_string(),
                    NodeSemantics::Branch,
                );
                self.connect_all(&iter_exits, branch);

                let body_entry =
                    self.push_node(node.span, "for-in body".to_string(), NodeSemantics::Marker);
                self.connect(branch, body_entry);
                let body_exits = self.build_block(body, vec![body_entry]);
                self.connect_all(&body_exits, branch);

                let after =
                    self.push_node(node.span, "for-in exit".to_string(), NodeSemantics::Marker);
                self.connect(branch, after);
                vec![after]
            }
            Node::WhileLoop { condition, body } => {
                let cond_exits = self.build_expr(condition, incoming);
                let branch = self.push_node(
                    node.span,
                    "while condition".to_string(),
                    NodeSemantics::Branch,
                );
                self.connect_all(&cond_exits, branch);

                let body_entry =
                    self.push_node(node.span, "while body".to_string(), NodeSemantics::Marker);
                self.connect(branch, body_entry);
                let body_exits = self.build_block(body, vec![body_entry]);
                self.connect_all(&body_exits, branch);

                let after =
                    self.push_node(node.span, "while exit".to_string(), NodeSemantics::Marker);
                self.connect(branch, after);
                vec![after]
            }
            Node::Retry { count, body } => {
                let count_exits = self.build_expr(count, incoming);
                let branch = self.push_node(
                    node.span,
                    "retry iteration".to_string(),
                    NodeSemantics::Branch,
                );
                self.connect_all(&count_exits, branch);

                let body_entry =
                    self.push_node(node.span, "retry body".to_string(), NodeSemantics::Marker);
                self.connect(branch, body_entry);
                let body_exits = self.build_block(body, vec![body_entry]);
                self.connect_all(&body_exits, branch);

                let after =
                    self.push_node(node.span, "retry exit".to_string(), NodeSemantics::Marker);
                self.connect(branch, after);
                vec![after]
            }
            Node::Parallel { expr, body, .. } => {
                let expr_exits = self.build_expr(expr, incoming);
                let branch = self.push_node(
                    node.span,
                    "parallel dispatch".to_string(),
                    NodeSemantics::Branch,
                );
                self.connect_all(&expr_exits, branch);
                let body_entry = self.push_node(
                    node.span,
                    "parallel body".to_string(),
                    NodeSemantics::Marker,
                );
                self.connect(branch, body_entry);
                let body_exits = self.build_block(body, vec![body_entry]);
                let after = self.push_node(
                    node.span,
                    "parallel join".to_string(),
                    NodeSemantics::Marker,
                );
                self.connect_all(&body_exits, after);
                self.connect(branch, after);
                vec![after]
            }
            Node::MatchExpr { value, arms } => {
                let value_exits = self.build_expr(value, incoming);
                let branch =
                    self.push_node(node.span, "match value".to_string(), NodeSemantics::Branch);
                self.connect_all(&value_exits, branch);
                let mut exits = Vec::new();
                for arm in arms {
                    let entry = self.push_node(
                        arm.pattern.span,
                        format!("match arm {}", pattern_label(&arm.pattern)),
                        NodeSemantics::Marker,
                    );
                    self.connect(branch, entry);
                    let arm_exits = if let Some(guard) = &arm.guard {
                        self.build_expr(guard, vec![entry])
                    } else {
                        vec![entry]
                    };
                    exits.extend(self.build_block(&arm.body, arm_exits));
                }
                exits
            }
            Node::TryCatch {
                body,
                catch_body,
                finally_body,
                ..
            } => {
                let branch =
                    self.push_node(node.span, "try dispatch".to_string(), NodeSemantics::Branch);
                self.connect_all(&incoming, branch);

                let try_entry =
                    self.push_node(node.span, "try body".to_string(), NodeSemantics::Marker);
                self.connect(branch, try_entry);
                let mut exits = self.build_block(body, vec![try_entry]);

                let catch_entry =
                    self.push_node(node.span, "catch body".to_string(), NodeSemantics::Marker);
                self.connect(branch, catch_entry);
                exits.extend(self.build_block(catch_body, vec![catch_entry]));

                if let Some(finally_body) = finally_body {
                    let finally_entry = self.push_node(
                        node.span,
                        "finally body".to_string(),
                        NodeSemantics::Marker,
                    );
                    self.connect_all(&exits, finally_entry);
                    return self.build_block(finally_body, vec![finally_entry]);
                }

                exits
            }
            Node::TryExpr { body }
            | Node::SpawnExpr { body }
            | Node::DeferStmt { body }
            | Node::MutexBlock { body }
            | Node::Block(body) => self.build_block(body, incoming),
            Node::DeadlineBlock { duration, body } => {
                let duration_exits = self.build_expr(duration, incoming);
                self.build_block(body, duration_exits)
            }
            Node::SelectExpr {
                cases,
                timeout,
                default_body,
            } => {
                let branch = self.push_node(node.span, "select".to_string(), NodeSemantics::Branch);
                self.connect_all(&incoming, branch);
                let mut exits = Vec::new();
                for case in cases {
                    let case_entry = self.push_node(
                        case.channel.span,
                        format!("select case {}", case.variable),
                        NodeSemantics::Marker,
                    );
                    self.connect(branch, case_entry);
                    let case_exits = self.build_expr(&case.channel, vec![case_entry]);
                    exits.extend(self.build_block(&case.body, case_exits));
                }
                if let Some((timeout_expr, timeout_body)) = timeout {
                    let timeout_entry = self.push_node(
                        timeout_expr.span,
                        "select timeout".to_string(),
                        NodeSemantics::Marker,
                    );
                    self.connect(branch, timeout_entry);
                    let timeout_exits = self.build_expr(timeout_expr, vec![timeout_entry]);
                    exits.extend(self.build_block(timeout_body, timeout_exits));
                }
                if let Some(default_body) = default_body {
                    let default_entry = self.push_node(
                        node.span,
                        "select default".to_string(),
                        NodeSemantics::Marker,
                    );
                    self.connect(branch, default_entry);
                    exits.extend(self.build_block(default_body, vec![default_entry]));
                }
                exits
            }
            Node::ReturnStmt { value } => {
                let exits = if let Some(value) = value.as_ref() {
                    self.build_expr(value, incoming)
                } else {
                    incoming
                };
                let ret = self.push_node(node.span, "return".to_string(), NodeSemantics::Return);
                self.connect_all(&exits, ret);
                Vec::new()
            }
            Node::ThrowStmt { value } => {
                let exits = self.build_expr(value, incoming);
                let throw = self.push_node(node.span, "throw".to_string(), NodeSemantics::Throw);
                self.connect_all(&exits, throw);
                Vec::new()
            }
            _ => self.build_expr(node, incoming),
        }
    }

    fn build_expr(&mut self, node: &SNode, incoming: Vec<NodeId>) -> Vec<NodeId> {
        match &node.node {
            Node::FunctionCall { name, args } => {
                self.build_function_call(node, name, args, incoming)
            }
            Node::MethodCall { object, args, .. }
            | Node::OptionalMethodCall { object, args, .. } => {
                let mut exits = self.build_expr(object, incoming);
                for arg in args {
                    exits = self.build_expr(arg, exits);
                }
                exits
            }
            Node::PropertyAccess { object, .. }
            | Node::OptionalPropertyAccess { object, .. }
            | Node::Spread(object)
            | Node::TryOperator { operand: object }
            | Node::TryStar { operand: object }
            | Node::UnaryOp {
                operand: object, ..
            } => self.build_expr(object, incoming),
            Node::SubscriptAccess { object, index } => {
                let exits = self.build_expr(object, incoming);
                self.build_expr(index, exits)
            }
            Node::SliceAccess { object, start, end } => {
                let mut exits = self.build_expr(object, incoming);
                if let Some(start) = start {
                    exits = self.build_expr(start, exits);
                }
                if let Some(end) = end {
                    exits = self.build_expr(end, exits);
                }
                exits
            }
            Node::BinaryOp { left, right, .. } => {
                let exits = self.build_expr(left, incoming);
                self.build_expr(right, exits)
            }
            Node::Ternary {
                condition,
                true_expr,
                false_expr,
            } => {
                let cond_exits = self.build_expr(condition, incoming);
                let branch = self.push_node(
                    node.span,
                    "ternary condition".to_string(),
                    NodeSemantics::Branch,
                );
                self.connect_all(&cond_exits, branch);
                let true_entry =
                    self.push_node(node.span, "ternary true".to_string(), NodeSemantics::Marker);
                self.connect(branch, true_entry);
                let false_entry = self.push_node(
                    node.span,
                    "ternary false".to_string(),
                    NodeSemantics::Marker,
                );
                self.connect(branch, false_entry);
                let mut exits = self.build_expr(true_expr, vec![true_entry]);
                exits.extend(self.build_expr(false_expr, vec![false_entry]));
                exits
            }
            Node::ListLiteral(items) | Node::OrPattern(items) => {
                let mut exits = incoming;
                for item in items {
                    exits = self.build_expr(item, exits);
                }
                exits
            }
            Node::DictLiteral(entries)
            | Node::StructConstruct {
                fields: entries, ..
            } => {
                let mut exits = incoming;
                for entry in entries {
                    exits = self.build_expr(&entry.key, exits);
                    exits = self.build_expr(&entry.value, exits);
                }
                exits
            }
            Node::EnumConstruct { args, .. } => {
                let mut exits = incoming;
                for arg in args {
                    exits = self.build_expr(arg, exits);
                }
                exits
            }
            Node::Block(body) => self.build_block(body, incoming),
            Node::MatchExpr { .. } => self.build_stmt(node, incoming),
            Node::Closure { .. } => incoming,
            _ => incoming,
        }
    }

    fn build_function_call(
        &mut self,
        node: &SNode,
        name: &str,
        args: &[SNode],
        incoming: Vec<NodeId>,
    ) -> Vec<NodeId> {
        if name == "dual_control" {
            let mut exits = incoming;
            for (index, arg) in args.iter().enumerate() {
                if index == 2 && matches!(arg.node, Node::Closure { .. }) {
                    continue;
                }
                exits = self.build_expr(arg, exits);
            }
            let enter = self.push_node(
                node.span,
                "dual_control approval gate".to_string(),
                NodeSemantics::ApprovalScopeEnter,
            );
            self.connect_all(&exits, enter);
            let closure_exits = match args.get(2) {
                Some(SNode {
                    node: Node::Closure { body, .. },
                    ..
                }) => self.build_block(body, vec![enter]),
                _ => vec![enter],
            };
            let exit = self.push_node(
                node.span,
                "end dual_control".to_string(),
                NodeSemantics::ApprovalScopeExit,
            );
            self.connect_all(&closure_exits, exit);
            return vec![exit];
        }

        let mut exits = incoming;
        for arg in args {
            exits = self.build_expr(arg, exits);
        }
        let call = classify_call(name, args);
        let call_id = self.push_node(
            node.span,
            format!("call {}", call.display_name),
            NodeSemantics::Call(call),
        );
        self.connect_all(&exits, call_id);
        vec![call_id]
    }
}

fn classify_call(name: &str, args: &[SNode]) -> CallSemantics {
    let literal_args = args.iter().map(literal_value).collect::<Vec<_>>();
    let mut display_name = name.to_string();
    let classification = match name {
        "request_approval" => CallClassification::ApprovalGate,
        "llm_budget_remaining" => CallClassification::BudgetRead,
        "write_file" | "append_file" | "delete_file" | "mkdir" | "apply_edit" => {
            let path = literal_args
                .first()
                .and_then(LiteralValue::as_str)
                .map(str::to_string);
            CallClassification::FsWrite { path }
        }
        "copy_file" => {
            let path = literal_args
                .get(1)
                .and_then(LiteralValue::as_str)
                .map(str::to_string);
            CallClassification::FsWrite { path }
        }
        "exec" | "exec_at" | "shell" | "shell_at" | "http_post" | "http_put" | "http_patch"
        | "http_delete" | "http_request" => CallClassification::SideEffect,
        "mcp_call" => {
            let tool_name = literal_args
                .get(1)
                .and_then(LiteralValue::as_str)
                .map(str::to_string);
            if let Some(tool_name) = tool_name {
                display_name = tool_name.clone();
                classify_tool_call(&tool_name, literal_args.get(2))
            } else {
                CallClassification::Other
            }
        }
        "host_tool_call" => {
            let tool_name = literal_args
                .first()
                .and_then(LiteralValue::as_str)
                .map(str::to_string);
            if let Some(tool_name) = tool_name {
                display_name = tool_name.clone();
                classify_tool_call(&tool_name, literal_args.get(1))
            } else {
                CallClassification::Other
            }
        }
        _ => CallClassification::Other,
    };

    CallSemantics {
        name: name.to_string(),
        display_name,
        classification,
        literal_args,
    }
}

fn classify_tool_call(tool_name: &str, args: Option<&LiteralValue>) -> CallClassification {
    let normalized = tool_name.to_ascii_lowercase();
    let path = args.and_then(extract_path_from_tool_args);
    if matches!(
        normalized.as_str(),
        "write_file"
            | "append_file"
            | "copy_file"
            | "delete_file"
            | "mkdir"
            | "apply_edit"
            | "write"
            | "edit"
            | "delete"
            | "move"
            | "rename"
            | "patch"
    ) || normalized.contains("write")
        || normalized.contains("edit")
        || normalized.contains("delete")
        || normalized.contains("move")
        || normalized.contains("rename")
        || normalized.contains("patch")
    {
        return CallClassification::FsWrite { path };
    }
    if normalized.contains("exec")
        || normalized.contains("shell")
        || normalized.contains("run")
        || normalized.contains("push_pr")
        || normalized.contains("create_pr")
        || normalized.contains("deploy")
    {
        return CallClassification::SideEffect;
    }
    CallClassification::Other
}

fn extract_path_from_tool_args(value: &LiteralValue) -> Option<String> {
    for key in ["path", "dst", "destination", "target"] {
        if let Some(path) = value.dict_field(key).and_then(LiteralValue::as_str) {
            return Some(path.to_string());
        }
    }
    None
}

fn literal_value(node: &SNode) -> LiteralValue {
    match &node.node {
        Node::StringLiteral(value) | Node::RawStringLiteral(value) => {
            LiteralValue::String(value.clone())
        }
        Node::Identifier(value) => LiteralValue::Identifier(value.clone()),
        Node::IntLiteral(value) => LiteralValue::Number(value.to_string()),
        Node::FloatLiteral(value) => LiteralValue::Number(value.to_string()),
        Node::BoolLiteral(value) => LiteralValue::Bool(*value),
        Node::NilLiteral => LiteralValue::Nil,
        Node::DictLiteral(entries)
        | Node::StructConstruct {
            fields: entries, ..
        } => {
            let mut map = BTreeMap::new();
            for entry in entries {
                if let Some(key) = literal_key(&entry.key) {
                    map.insert(key, literal_value(&entry.value));
                }
            }
            LiteralValue::Dict(map)
        }
        Node::ListLiteral(items) => LiteralValue::List(items.iter().map(literal_value).collect()),
        _ => LiteralValue::Unknown,
    }
}

fn literal_key(node: &SNode) -> Option<String> {
    match &node.node {
        Node::StringLiteral(value) | Node::RawStringLiteral(value) | Node::Identifier(value) => {
            Some(value.clone())
        }
        _ => None,
    }
}

fn expr_summary(node: &SNode) -> ExprSummary {
    match &node.node {
        Node::Identifier(name) => ExprSummary::Reference(name.clone()),
        Node::PropertyAccess { .. } | Node::OptionalPropertyAccess { .. } => target_path(node)
            .map(ExprSummary::Reference)
            .unwrap_or(ExprSummary::Unknown),
        Node::FunctionCall { name, .. } => ExprSummary::Call(name.clone()),
        Node::BinaryOp { op, left, right } => ExprSummary::Binary {
            op: op.clone(),
            left: Box::new(expr_summary(left)),
            right: Box::new(expr_summary(right)),
        },
        Node::IntLiteral(_)
        | Node::FloatLiteral(_)
        | Node::StringLiteral(_)
        | Node::RawStringLiteral(_)
        | Node::BoolLiteral(_)
        | Node::NilLiteral => ExprSummary::Literal,
        _ => ExprSummary::Unknown,
    }
}

fn assignment_is_non_increasing(assignment: &AssignmentSemantics, target: &str) -> bool {
    match assignment.op.as_deref() {
        Some("-") => true,
        Some("+") | Some("*") | Some("/") | Some("%") => false,
        Some(_) => false,
        None => match &assignment.value {
            ExprSummary::Reference(value) => value == target,
            ExprSummary::Call(name) => name == "llm_budget_remaining",
            ExprSummary::Binary { op, left, .. } if op == "-" => {
                matches!(left.as_ref(), ExprSummary::Reference(value) if value == target)
            }
            _ => false,
        },
    }
}

fn path_to_node(ir: &HandlerIr, target: NodeId) -> Vec<PathStep> {
    let mut queue = VecDeque::new();
    let mut seen = HashSet::new();
    queue.push_back((ir.entry, vec![ir.entry]));

    while let Some((node, path)) = queue.pop_front() {
        if node == target {
            return path
                .into_iter()
                .map(|id| {
                    let node = ir.node(id);
                    PathStep {
                        span: node.span,
                        label: node.label.clone(),
                    }
                })
                .collect();
        }
        if !seen.insert(node) {
            continue;
        }
        for succ in ir.successors(node) {
            let mut next_path = path.clone();
            next_path.push(succ);
            queue.push_back((succ, next_path));
        }
    }

    Vec::new()
}

fn target_path(node: &SNode) -> Option<String> {
    match &node.node {
        Node::Identifier(name) => Some(name.clone()),
        Node::PropertyAccess { object, property }
        | Node::OptionalPropertyAccess { object, property } => {
            let base = target_path(object)?;
            Some(format!("{base}.{property}"))
        }
        _ => None,
    }
}

fn pattern_label(node: &SNode) -> String {
    match &node.node {
        Node::StringLiteral(value) | Node::RawStringLiteral(value) => format!("{value:?}"),
        Node::Identifier(value) => value.clone(),
        Node::IntLiteral(value) => value.to_string(),
        Node::BoolLiteral(value) => value.to_string(),
        Node::NilLiteral => "nil".to_string(),
        Node::OrPattern(_) => "or-pattern".to_string(),
        _ => "pattern".to_string(),
    }
}

fn glob_match(pattern: &str, path: &str) -> bool {
    fn helper(pattern: &[u8], pi: usize, path: &[u8], si: usize) -> bool {
        if pi == pattern.len() {
            return si == path.len();
        }

        if pattern[pi] == b'*' {
            if pattern.get(pi + 1) == Some(&b'*') {
                let next = pi + 2;
                if next == pattern.len() {
                    return true;
                }
                for index in si..=path.len() {
                    if helper(pattern, next, path, index) {
                        return true;
                    }
                }
                return false;
            }

            let next = pi + 1;
            let mut index = si;
            while index <= path.len() {
                if helper(pattern, next, path, index) {
                    return true;
                }
                if index == path.len() || path[index] == b'/' {
                    break;
                }
                index += 1;
            }
            return false;
        }

        if si == path.len() || pattern[pi] != path[si] {
            return false;
        }
        helper(pattern, pi + 1, path, si + 1)
    }

    helper(pattern.as_bytes(), 0, path.as_bytes(), 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_program(source: &str) -> Vec<SNode> {
        let mut lexer = harn_lexer::Lexer::new(source);
        let tokens = lexer.tokenize().expect("tokenize");
        let mut parser = harn_parser::Parser::new(tokens);
        parser.parse().expect("parse")
    }

    fn analyze(source: &str) -> AnalysisReport {
        analyze_program(&parse_program(source))
    }

    fn diagnostics_by_invariant<'a>(
        report: &'a AnalysisReport,
        invariant: &str,
    ) -> Vec<&'a InvariantDiagnostic> {
        report
            .diagnostics
            .iter()
            .filter(|diag| diag.invariant == invariant)
            .collect()
    }

    #[test]
    fn fs_writes_within_glob_passes() {
        let report = analyze(
            r#"
@invariant("fs.writes", "src/**")
fn handler() {
  write_file("src/main.rs", "ok")
}
"#,
        );

        assert!(
            diagnostics_by_invariant(&report, "fs.writes").is_empty(),
            "unexpected diagnostics: {:?}",
            report.diagnostics
        );
    }

    #[test]
    fn fs_writes_outside_glob_fails() {
        let report = analyze(
            r#"
@invariant("fs.writes", "src/**")
fn handler() {
  write_file("/tmp/main.rs", "nope")
}
"#,
        );

        let diags = diagnostics_by_invariant(&report, "fs.writes");
        assert_eq!(diags.len(), 1);
        assert!(diags[0].message.contains("outside the allowed glob"));
        assert!(diags[0]
            .path
            .iter()
            .any(|step| step.label.contains("write_file")));
    }

    #[test]
    fn approval_requires_gate_on_all_paths() {
        let report = analyze(
            r#"
@invariant("approval.reachability")
fn handler() {
  if true {
    request_approval("ship it")
  }
  write_file("src/main.rs", "unsafe")
}
"#,
        );

        let diags = diagnostics_by_invariant(&report, "approval.reachability");
        assert_eq!(diags.len(), 1);
        assert!(diags[0].message.contains("before any approval gate"));
    }

    #[test]
    fn approval_inside_dual_control_closure_is_accepted() {
        let report = analyze(
            r#"
@invariant("approval.reachability")
fn handler() {
  dual_control(2, 3, { ->
    write_file("src/main.rs", "safe")
  }, ["alice", "bob", "carol"])
}
"#,
        );

        assert!(
            diagnostics_by_invariant(&report, "approval.reachability").is_empty(),
            "unexpected diagnostics: {:?}",
            report.diagnostics
        );
    }

    #[test]
    fn budget_remaining_rejects_addition() {
        let report = analyze(
            r#"
@invariant("budget.remaining", target: "remaining")
fn handler() {
  let remaining = llm_budget_remaining()
  remaining = remaining + 1
}
"#,
        );

        let diags = diagnostics_by_invariant(&report, "budget.remaining");
        assert_eq!(diags.len(), 1);
        assert!(diags[0].message.contains("may increase"));
    }

    #[test]
    fn budget_remaining_accepts_subtraction() {
        let report = analyze(
            r#"
@invariant("budget.remaining", target: "remaining")
fn handler(cost) {
  let remaining = llm_budget_remaining()
  remaining -= cost
}
"#,
        );

        assert!(
            diagnostics_by_invariant(&report, "budget.remaining").is_empty(),
            "unexpected diagnostics: {:?}",
            report.diagnostics
        );
    }

    #[test]
    fn explain_returns_violation_path() {
        let diags = explain_handler_invariant(
            &parse_program(
                r#"
@invariant("approval.reachability")
fn handler() {
  write_file("src/main.rs", "unsafe")
}
"#,
            ),
            "handler",
            "approval.reachability",
        )
        .expect("explain succeeds");

        assert_eq!(diags.len(), 1);
        assert!(diags[0].path.len() >= 2);
    }

    #[test]
    fn glob_match_supports_single_and_double_star() {
        assert!(glob_match("src/*.rs", "src/main.rs"));
        assert!(!glob_match("src/*.rs", "src/nested/main.rs"));
        assert!(glob_match("src/**/*.rs", "src/nested/main.rs"));
    }
}
