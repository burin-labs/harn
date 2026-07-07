use std::collections::{BTreeMap, BTreeSet, HashSet, VecDeque};

use harn_lexer::Span;
use harn_parser::{Attribute, AttributeArg, BindingPattern, HitlArg, HitlKind, Node, SNode};

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
    PolicyScopeEnter(PolicyScopeKind),
    PolicyScopeExit(PolicyScopeKind),
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
    PolicyGate(PolicyScopeKind),
    PolicyPush(PolicyScopeKind),
    PolicyPop(PolicyScopeKind),
    Capabilities(Vec<CapabilityEffect>),
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Capability {
    WorkspaceMutation,
    CommandExecution,
    NetworkAccess,
    ConnectorAccess,
    ModelCall,
    WorkerDispatch,
    HumanApproval,
    AutonomyPolicy,
}

impl Capability {
    fn canonical(self) -> &'static str {
        match self {
            Self::WorkspaceMutation => "fs.write",
            Self::CommandExecution => "process.exec",
            Self::NetworkAccess => "network.access",
            Self::ConnectorAccess => "mcp.connector",
            Self::ModelCall => "llm.model",
            Self::WorkerDispatch => "worker.dispatch",
            Self::HumanApproval => "human.approval",
            Self::AutonomyPolicy => "autonomy.policy",
        }
    }

    fn from_policy_name(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "fs.write" | "fs.writes" | "workspace.write" | "workspace.mutate"
            | "workspace.mutation" | "filesystem.write" | "filesystem.mutate" => {
                Some(Self::WorkspaceMutation)
            }
            "process.exec" | "command.exec" | "command" | "exec" | "shell" => {
                Some(Self::CommandExecution)
            }
            "network.access" | "network" | "http" | "sse" | "websocket" => {
                Some(Self::NetworkAccess)
            }
            "mcp.connector" | "connector" | "connectors" | "mcp" | "host.tool" | "host_tool" => {
                Some(Self::ConnectorAccess)
            }
            "llm.model" | "model" | "llm" | "model.call" => Some(Self::ModelCall),
            "worker.dispatch" | "worker" | "delegated.worker" | "a2a" => Some(Self::WorkerDispatch),
            "human.approval" | "approval" | "hitl" => Some(Self::HumanApproval),
            "autonomy.policy" | "autonomy" => Some(Self::AutonomyPolicy),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapabilityEffect {
    pub capability: Capability,
    pub operation: String,
    pub path: Option<String>,
}

impl CapabilityEffect {
    fn new(capability: Capability, operation: impl Into<String>, path: Option<String>) -> Self {
        Self {
            capability,
            operation: operation.into(),
            path,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PolicyScopeKind {
    Execution,
    ToolApproval,
    Command,
    Egress,
    Autonomy,
    DynamicPermissions,
}

impl PolicyScopeKind {
    fn label(self) -> &'static str {
        match self {
            Self::Execution => "execution policy",
            Self::ToolApproval => "approval policy",
            Self::Command => "command policy",
            Self::Egress => "egress policy",
            Self::Autonomy => "autonomy policy",
            Self::DynamicPermissions => "dynamic permissions",
        }
    }
}

impl CallSemantics {
    fn capability_effects(&self) -> &[CapabilityEffect] {
        match &self.classification {
            CallClassification::Capabilities(effects) => effects,
            _ => &[],
        }
    }

    fn has_budget_option(&self) -> bool {
        self.literal_args.iter().any(literal_has_budget_policy)
    }
}

fn literal_has_budget_policy(value: &LiteralValue) -> bool {
    match value {
        LiteralValue::Dict(entries) => entries.iter().any(|(key, value)| {
            key == "budget" || key == "token_budget" || literal_has_budget_policy(value)
        }),
        LiteralValue::List(items) => items.iter().any(literal_has_budget_policy),
        _ => false,
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

#[derive(Debug, Clone)]
pub struct CapabilityPolicyInvariant {
    allowed: BTreeSet<Capability>,
    workspace_globs: Vec<String>,
    require_approval: BTreeSet<Capability>,
    require_budget: BTreeSet<Capability>,
    require_autonomy: BTreeSet<Capability>,
    require_execution_policy: BTreeSet<Capability>,
    require_command_policy: BTreeSet<Capability>,
    require_egress_policy: BTreeSet<Capability>,
    require_approval_policy: BTreeSet<Capability>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct CapabilityPolicyState {
    explicit_approval: bool,
    scoped_approval_depth: u8,
    execution_policy_depth: u8,
    approval_policy_depth: u8,
    command_policy_depth: u8,
    egress_policy_depth: u8,
    autonomy_policy_depth: u8,
    dynamic_permissions_depth: u8,
    egress_policy_seen: bool,
    budget_seen: bool,
}

impl CapabilityPolicyState {
    fn initial() -> Self {
        Self {
            explicit_approval: false,
            scoped_approval_depth: 0,
            execution_policy_depth: 0,
            approval_policy_depth: 0,
            command_policy_depth: 0,
            egress_policy_depth: 0,
            autonomy_policy_depth: 0,
            dynamic_permissions_depth: 0,
            egress_policy_seen: false,
            budget_seen: false,
        }
    }

    fn is_approved(self) -> bool {
        self.explicit_approval || self.scoped_approval_depth > 0
    }

    fn has_execution_policy(self) -> bool {
        self.execution_policy_depth > 0 || self.dynamic_permissions_depth > 0
    }

    fn has_command_policy(self) -> bool {
        self.command_policy_depth > 0 || self.has_execution_policy()
    }

    fn has_egress_policy(self) -> bool {
        self.egress_policy_depth > 0 || self.egress_policy_seen || self.has_execution_policy()
    }

    fn has_autonomy_policy(self) -> bool {
        self.autonomy_policy_depth > 0
    }

    fn has_approval_policy(self) -> bool {
        self.approval_policy_depth > 0
    }
}

struct CapabilityCheckContext<'a, 'b> {
    ir: &'a HandlerIr,
    node: &'a IrNode,
    call: &'a CallSemantics,
    effect: &'a CapabilityEffect,
    path: &'a [PathStep],
    reported: &'b mut BTreeSet<(NodeId, Capability, &'static str)>,
    diagnostics: &'b mut Vec<InvariantDiagnostic>,
}

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
            let Some(effect) = call
                .capability_effects()
                .iter()
                .find(|effect| effect.capability == Capability::WorkspaceMutation)
            else {
                continue;
            };

            let message = match effect.path.as_deref() {
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
                NodeSemantics::Call(call) => match &call.classification {
                    CallClassification::ApprovalGate => {
                        next_state.explicit_approval = true;
                    }
                    CallClassification::Capabilities(effects) => {
                        for effect in effects {
                            if state.is_approved() || !reported.insert((node_id, effect.capability))
                            {
                                continue;
                            }
                            diagnostics.push(InvariantDiagnostic {
                                invariant: self.name().to_string(),
                                handler: ir.name.clone(),
                                message: format!(
                                    "side-effecting call `{}` for capability `{}` is reachable before any approval gate",
                                    call.display_name,
                                    effect.capability.canonical()
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

impl Invariant for CapabilityPolicyInvariant {
    fn name(&self) -> &'static str {
        "capability.policy"
    }

    fn check(&self, ir: &HandlerIr) -> Vec<InvariantDiagnostic> {
        let mut diagnostics = Vec::new();
        let mut queue = VecDeque::new();
        let mut visited = HashSet::new();
        let mut reported = BTreeSet::new();

        queue.push_back((
            ir.entry,
            CapabilityPolicyState::initial(),
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
                NodeSemantics::Call(call) => match &call.classification {
                    CallClassification::ApprovalGate => next_state.explicit_approval = true,
                    CallClassification::BudgetRead => next_state.budget_seen = true,
                    CallClassification::PolicyGate(PolicyScopeKind::Egress) => {
                        next_state.egress_policy_seen = true;
                    }
                    CallClassification::PolicyGate(_) => {}
                    CallClassification::PolicyPush(kind) => {
                        increment_policy_depth(&mut next_state, *kind);
                    }
                    CallClassification::PolicyPop(kind) => {
                        decrement_policy_depth(&mut next_state, *kind);
                    }
                    CallClassification::Capabilities(effects) => {
                        for effect in effects {
                            let mut context = CapabilityCheckContext {
                                ir,
                                node,
                                call,
                                effect,
                                path: &path,
                                reported: &mut reported,
                                diagnostics: &mut diagnostics,
                            };
                            self.check_effect(state, &mut context);
                        }
                    }
                    CallClassification::Other => {}
                },
                NodeSemantics::ApprovalScopeEnter => {
                    next_state.scoped_approval_depth =
                        next_state.scoped_approval_depth.saturating_add(1);
                }
                NodeSemantics::ApprovalScopeExit => {
                    next_state.scoped_approval_depth =
                        next_state.scoped_approval_depth.saturating_sub(1);
                }
                NodeSemantics::PolicyScopeEnter(kind) => {
                    increment_policy_depth(&mut next_state, *kind);
                }
                NodeSemantics::PolicyScopeExit(kind) => {
                    decrement_policy_depth(&mut next_state, *kind);
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

impl CapabilityPolicyInvariant {
    fn check_effect(
        &self,
        state: CapabilityPolicyState,
        context: &mut CapabilityCheckContext<'_, '_>,
    ) {
        let capability = context.effect.capability;
        if !self.allowed.contains(&capability)
            && context
                .reported
                .insert((context.node.id, capability, "allow"))
        {
            context.diagnostics.push(InvariantDiagnostic {
                invariant: self.name().to_string(),
                handler: context.ir.name.clone(),
                message: format!(
                    "handler `{}` can reach capability `{}` via `{}` but that capability is not declared in `@invariant(\"capability.policy\", allow: ...)`",
                    context.ir.name,
                    capability.canonical(),
                    context.effect.operation
                ),
                span: context.node.span,
                help: Some(format!(
                    "add `{}` to the invariant's `allow:` list or remove the reachable call",
                    capability.canonical()
                )),
                path: context.path.to_vec(),
            });
            return;
        }

        if capability == Capability::WorkspaceMutation {
            self.check_workspace_path(context);
        }
        self.check_required_gate(state, context);
    }

    fn check_workspace_path(&self, context: &mut CapabilityCheckContext<'_, '_>) {
        if self.workspace_globs.is_empty() {
            return;
        }
        let message = match context.effect.path.as_deref() {
            Some(path)
                if self
                    .workspace_globs
                    .iter()
                    .any(|glob| glob_match(glob, path)) =>
            {
                return;
            }
            Some(path) => format!(
                "handler `{}` can reach capability `{}` via `{}` with path `{path}` outside the allowed workspace glob(s): {}",
                context.ir.name,
                context.effect.capability.canonical(),
                context.call.display_name,
                self.workspace_globs.join(", ")
            ),
            None => format!(
                "handler `{}` can reach capability `{}` via `{}` but the target path is not a literal proven inside the allowed workspace glob(s): {}",
                context.ir.name,
                context.effect.capability.canonical(),
                context.call.display_name,
                self.workspace_globs.join(", ")
            ),
        };
        if context
            .reported
            .insert((context.node.id, context.effect.capability, "workspace"))
        {
            context.diagnostics.push(InvariantDiagnostic {
                invariant: self.name().to_string(),
                handler: context.ir.name.clone(),
                message,
                span: context.node.span,
                help: Some(
                    "use a literal path inside the declared workspace glob or narrow the policy"
                        .to_string(),
                ),
                path: context.path.to_vec(),
            });
        }
    }

    fn check_required_gate(
        &self,
        state: CapabilityPolicyState,
        context: &mut CapabilityCheckContext<'_, '_>,
    ) {
        let capability = context.effect.capability;
        if self.require_approval.contains(&capability) && !state.is_approved() {
            self.push_missing_gate(
                context,
                "approval",
                "human approval gate",
                "call `request_approval(...)` earlier on every path or wrap the action in `dual_control(...)`",
            );
        }
        if self.require_budget.contains(&capability)
            && !state.budget_seen
            && !context.call.has_budget_option()
        {
            self.push_missing_gate(
                context,
                "budget",
                "budget policy",
                "thread a `llm_budget_remaining()` check before the call or pass a literal `budget:` option",
            );
        }
        if self.require_autonomy.contains(&capability) && !state.has_autonomy_policy() {
            self.push_missing_gate(
                context,
                "autonomy",
                "autonomy policy",
                "wrap the reachable call in `with_autonomy_policy(...)`",
            );
        }
        if self.require_execution_policy.contains(&capability) && !state.has_execution_policy() {
            self.push_missing_gate(
                context,
                "execution",
                "execution policy",
                "wrap the reachable call in `with_execution_policy(...)` or `with_dynamic_permissions(...)`",
            );
        }
        if self.require_command_policy.contains(&capability) && !state.has_command_policy() {
            self.push_missing_gate(
                context,
                "command",
                "command policy",
                "wrap the reachable command in `with_command_policy(...)` or install `command_policy_push(...)` before it",
            );
        }
        if self.require_egress_policy.contains(&capability) && !state.has_egress_policy() {
            self.push_missing_gate(
                context,
                "egress",
                "egress policy",
                "install `egress_policy(...)` before the reachable network or connector call",
            );
        }
        if self.require_approval_policy.contains(&capability) && !state.has_approval_policy() {
            self.push_missing_gate(
                context,
                "approval_policy",
                "tool approval policy",
                "wrap the reachable tool call in `with_approval_policy(...)`",
            );
        }
    }

    fn push_missing_gate(
        &self,
        context: &mut CapabilityCheckContext<'_, '_>,
        gate_key: &'static str,
        gate_label: &'static str,
        help: &str,
    ) {
        if !context
            .reported
            .insert((context.node.id, context.effect.capability, gate_key))
        {
            return;
        }
        context.diagnostics.push(InvariantDiagnostic {
            invariant: self.name().to_string(),
            handler: context.ir.name.clone(),
            message: format!(
                "handler `{}` can reach capability `{}` via `{}` without the required {gate_label}",
                context.ir.name,
                context.effect.capability.canonical(),
                context.call.display_name
            ),
            span: context.node.span,
            help: Some(help.to_string()),
            path: context.path.to_vec(),
        });
    }
}

fn increment_policy_depth(state: &mut CapabilityPolicyState, kind: PolicyScopeKind) {
    match kind {
        PolicyScopeKind::Execution => {
            state.execution_policy_depth = state.execution_policy_depth.saturating_add(1);
        }
        PolicyScopeKind::ToolApproval => {
            state.approval_policy_depth = state.approval_policy_depth.saturating_add(1);
        }
        PolicyScopeKind::Command => {
            state.command_policy_depth = state.command_policy_depth.saturating_add(1);
        }
        PolicyScopeKind::Egress => {
            state.egress_policy_depth = state.egress_policy_depth.saturating_add(1);
        }
        PolicyScopeKind::Autonomy => {
            state.autonomy_policy_depth = state.autonomy_policy_depth.saturating_add(1);
        }
        PolicyScopeKind::DynamicPermissions => {
            state.dynamic_permissions_depth = state.dynamic_permissions_depth.saturating_add(1);
        }
    }
}

fn decrement_policy_depth(state: &mut CapabilityPolicyState, kind: PolicyScopeKind) {
    match kind {
        PolicyScopeKind::Execution => {
            state.execution_policy_depth = state.execution_policy_depth.saturating_sub(1);
        }
        PolicyScopeKind::ToolApproval => {
            state.approval_policy_depth = state.approval_policy_depth.saturating_sub(1);
        }
        PolicyScopeKind::Command => {
            state.command_policy_depth = state.command_policy_depth.saturating_sub(1);
        }
        PolicyScopeKind::Egress => {
            state.egress_policy_depth = state.egress_policy_depth.saturating_sub(1);
        }
        PolicyScopeKind::Autonomy => {
            state.autonomy_policy_depth = state.autonomy_policy_depth.saturating_sub(1);
        }
        PolicyScopeKind::DynamicPermissions => {
            state.dynamic_permissions_depth = state.dynamic_permissions_depth.saturating_sub(1);
        }
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
                diagnostics.push(*diag);
            }
        }
    }

    (specs, diagnostics)
}

fn parse_invariant_spec(attribute: &Attribute) -> Result<InvariantSpec, Box<InvariantDiagnostic>> {
    let mut named = BTreeMap::new();
    let mut positionals = Vec::new();

    for arg in &attribute.args {
        let Some(value) = attribute_arg_string(arg) else {
            return Err(Box::new(InvariantDiagnostic {
                invariant: "invariant".to_string(),
                handler: String::new(),
                message: "`@invariant(...)` arguments must be strings, identifiers, numbers, bools, or nil".to_string(),
                span: arg.span,
                help: Some("use strings for invariant names and configuration values".to_string()),
                path: Vec::new(),
            }));
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
        .ok_or_else(|| Box::new(InvariantDiagnostic {
            invariant: "invariant".to_string(),
            handler: String::new(),
            message: "`@invariant(...)` requires an invariant name as the first positional argument or `name:`".to_string(),
            span: attribute.span,
            help: Some(
                "for example: `@invariant(\"fs.writes\", \"src/**\")`".to_string(),
            ),
            path: Vec::new(),
        }))?;
    let name = normalize_invariant_name(&raw_name).ok_or_else(|| {
        Box::new(InvariantDiagnostic {
            invariant: raw_name.clone(),
            handler: String::new(),
            message: format!("unknown invariant `{raw_name}`"),
            span: attribute.span,
            help: Some(
                "known invariants are `fs.writes`, `budget.remaining`, `approval.reachability`, and `capability.policy`"
                    .to_string(),
            ),
            path: Vec::new(),
        })
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
        "capability.policy" | "capability_policy" | "capabilities" | "policy.capabilities" => {
            Some("capability.policy".to_string())
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
        "capability.policy" => instantiate_capability_policy_invariant(spec),
        other => Err(ConfigDiagnosticBuilder::new(
            other,
            spec.span,
            format!("unknown invariant `{other}`"),
            None,
        )),
    }
}

fn instantiate_capability_policy_invariant(
    spec: &InvariantSpec,
) -> Result<Box<dyn Invariant>, ConfigDiagnosticBuilder> {
    let allow_raw = spec
        .params
        .get("allow")
        .or_else(|| spec.params.get("capabilities"))
        .or_else(|| spec.params.get("allow_capabilities"))
        .or_else(|| spec.positionals.first())
        .ok_or_else(|| {
            ConfigDiagnosticBuilder::new(
                "capability.policy",
                spec.span,
                "`capability.policy` requires an `allow:` capability list".to_string(),
                Some(
                    "for example: `@invariant(\"capability.policy\", allow: \"fs.write,llm.model\")`"
                        .to_string(),
                ),
            )
        })?;
    let allowed = parse_capability_set(allow_raw).map_err(|message| {
        ConfigDiagnosticBuilder::new("capability.policy", spec.span, message, capability_help())
    })?;
    if allowed.is_empty() {
        return Err(ConfigDiagnosticBuilder::new(
            "capability.policy",
            spec.span,
            "`capability.policy` allow list must contain at least one capability".to_string(),
            capability_help(),
        ));
    }

    let workspace_globs = collect_named_values(
        spec,
        &[
            "workspace",
            "workspace_glob",
            "path_glob",
            "glob",
            "allow_workspace",
        ],
    );

    Ok(Box::new(CapabilityPolicyInvariant {
        allowed,
        workspace_globs,
        require_approval: parse_optional_capability_set(spec, &["require_approval"])?,
        require_budget: parse_optional_capability_set(spec, &["require_budget", "budget"])?,
        require_autonomy: parse_optional_capability_set(spec, &["require_autonomy"])?,
        require_execution_policy: parse_optional_capability_set(
            spec,
            &["require_execution_policy", "require_sandbox"],
        )?,
        require_command_policy: parse_optional_capability_set(spec, &["require_command_policy"])?,
        require_egress_policy: parse_optional_capability_set(spec, &["require_egress_policy"])?,
        require_approval_policy: parse_optional_capability_set(spec, &["require_approval_policy"])?,
    }))
}

fn parse_optional_capability_set(
    spec: &InvariantSpec,
    keys: &[&str],
) -> Result<BTreeSet<Capability>, ConfigDiagnosticBuilder> {
    let Some(raw) = keys.iter().find_map(|key| spec.params.get(*key)) else {
        return Ok(BTreeSet::new());
    };
    parse_capability_set(raw).map_err(|message| {
        ConfigDiagnosticBuilder::new("capability.policy", spec.span, message, capability_help())
    })
}

fn collect_named_values(spec: &InvariantSpec, keys: &[&str]) -> Vec<String> {
    keys.iter()
        .filter_map(|key| spec.params.get(*key).cloned())
        .flat_map(|value| split_config_list(&value))
        .collect()
}

fn parse_capability_set(raw: &str) -> Result<BTreeSet<Capability>, String> {
    let mut capabilities = BTreeSet::new();
    for item in split_config_list(raw) {
        let Some(capability) = Capability::from_policy_name(&item) else {
            return Err(format!(
                "unknown capability `{item}` in `capability.policy`"
            ));
        };
        capabilities.insert(capability);
    }
    Ok(capabilities)
}

fn split_config_list(raw: &str) -> Vec<String> {
    raw.split(|ch: char| ch == ',' || ch == ';' || ch.is_whitespace())
        .map(str::trim)
        .filter(|item| !item.is_empty())
        .map(str::to_string)
        .collect()
}

fn capability_help() -> Option<String> {
    Some(
        "known capabilities are `fs.write`, `process.exec`, `network.access`, `mcp.connector`, `llm.model`, `worker.dispatch`, `human.approval`, and `autonomy.policy`"
            .to_string(),
    )
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
            Node::LetBinding { pattern, value, .. } | Node::ConstBinding { pattern, value, .. } => {
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
                has_catch: _,
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
            | Node::MutexBlock { body, .. }
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
            Node::FunctionCall { name, args, .. } => {
                self.build_function_call(node, name, args, incoming)
            }
            Node::HitlExpr { kind, args } => self.build_hitl_expr(node, *kind, args, incoming),
            Node::MethodCall {
                object,
                method,
                args,
            }
            | Node::OptionalMethodCall {
                object,
                method,
                args,
            } => self.build_method_call(node, object, method, args, incoming),
            Node::PropertyAccess { object, .. }
            | Node::OptionalPropertyAccess { object, .. }
            | Node::Spread(object)
            | Node::TryOperator { operand: object }
            | Node::TryStar { operand: object }
            | Node::UnaryOp {
                operand: object, ..
            } => self.build_expr(object, incoming),
            Node::SubscriptAccess { object, index }
            | Node::OptionalSubscriptAccess { object, index } => {
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

        if let Some(scope) = scoped_policy_call(name) {
            return self.build_policy_scope_call(node, args, incoming, scope);
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

    /// Lower a method call. The common case is a pass-through (walk the
    /// receiver + args). When the receiver is a `harness.<sub_handle>`
    /// access, we synthesize the ambient builtin name so capability
    /// classification (`harn graph --json`, routes, IR analysis) sees
    /// `harness.fs.read_text("x")` as having the same effect surface as
    /// the legacy `read_file("x")`. This is the single place the IR
    /// needs to know about the `Harness` sub-handle shape; everything
    /// downstream (`direct_capabilities`, `classify_call`) keeps using
    /// the canonical ambient name table.
    fn build_method_call(
        &mut self,
        node: &SNode,
        object: &SNode,
        method: &str,
        args: &[SNode],
        incoming: Vec<NodeId>,
    ) -> Vec<NodeId> {
        let mut exits = self.build_expr(object, incoming);
        for arg in args {
            exits = self.build_expr(arg, exits);
        }
        if let Some((sub_handle, ambient)) = harness_sub_handle_for(object, method) {
            let call = CallSemantics {
                name: ambient.to_string(),
                display_name: format!("harness.{sub_handle}.{method}"),
                classification: classify_call(ambient, args).classification,
                literal_args: literal_args(args),
            };
            let call_id = self.push_node(
                node.span,
                format!("call {}", call.display_name),
                NodeSemantics::Call(call),
            );
            self.connect_all(&exits, call_id);
            return vec![call_id];
        }
        exits
    }

    fn build_policy_scope_call(
        &mut self,
        node: &SNode,
        args: &[SNode],
        incoming: Vec<NodeId>,
        scope: PolicyScopeKind,
    ) -> Vec<NodeId> {
        let closure_index = 1;
        let mut exits = incoming;
        for (index, arg) in args.iter().enumerate() {
            if index == closure_index && matches!(arg.node, Node::Closure { .. }) {
                continue;
            }
            exits = self.build_expr(arg, exits);
        }
        let enter = self.push_node(
            node.span,
            format!("enter {}", scope.label()),
            NodeSemantics::PolicyScopeEnter(scope),
        );
        self.connect_all(&exits, enter);
        let closure_exits = match args.get(closure_index) {
            Some(SNode {
                node: Node::Closure { body, .. },
                ..
            }) => self.build_block(body, vec![enter]),
            _ => vec![enter],
        };
        let exit = self.push_node(
            node.span,
            format!("exit {}", scope.label()),
            NodeSemantics::PolicyScopeExit(scope),
        );
        self.connect_all(&closure_exits, exit);
        vec![exit]
    }

    fn build_hitl_expr(
        &mut self,
        node: &SNode,
        kind: HitlKind,
        args: &[HitlArg],
        incoming: Vec<NodeId>,
    ) -> Vec<NodeId> {
        match kind {
            HitlKind::RequestApproval => {
                let mut exits = incoming;
                for arg in args {
                    exits = self.build_expr(&arg.value, exits);
                }
                let call = CallSemantics {
                    name: kind.as_keyword().to_string(),
                    display_name: kind.as_keyword().to_string(),
                    classification: CallClassification::ApprovalGate,
                    literal_args: args
                        .iter()
                        .map(|arg| literal_value(&arg.value))
                        .collect::<Vec<_>>(),
                };
                let call_id = self.push_node(
                    node.span,
                    format!("call {}", kind.as_keyword()),
                    NodeSemantics::Call(call),
                );
                self.connect_all(&exits, call_id);
                vec![call_id]
            }
            HitlKind::DualControl => self.build_hitl_dual_control(node, args, incoming),
            HitlKind::AskUser | HitlKind::EscalateTo => {
                let mut exits = incoming;
                for arg in args {
                    exits = self.build_expr(&arg.value, exits);
                }
                exits
            }
        }
    }

    fn build_hitl_dual_control(
        &mut self,
        node: &SNode,
        args: &[HitlArg],
        incoming: Vec<NodeId>,
    ) -> Vec<NodeId> {
        let closure_index = args
            .iter()
            .position(|arg| arg.name.as_deref() == Some("action"))
            .or(Some(2));
        let mut exits = incoming;
        for (index, arg) in args.iter().enumerate() {
            if Some(index) == closure_index && matches!(arg.value.node, Node::Closure { .. }) {
                continue;
            }
            exits = self.build_expr(&arg.value, exits);
        }
        let enter = self.push_node(
            node.span,
            "dual_control approval gate".to_string(),
            NodeSemantics::ApprovalScopeEnter,
        );
        self.connect_all(&exits, enter);
        let closure_exits = closure_index
            .and_then(|index| args.get(index))
            .and_then(|arg| match &arg.value {
                SNode {
                    node: Node::Closure { body, .. },
                    ..
                } => Some(self.build_block(body, vec![enter])),
                _ => None,
            })
            .unwrap_or_else(|| vec![enter]);
        let exit = self.push_node(
            node.span,
            "end dual_control".to_string(),
            NodeSemantics::ApprovalScopeExit,
        );
        self.connect_all(&closure_exits, exit);
        vec![exit]
    }
}

fn scoped_policy_call(name: &str) -> Option<PolicyScopeKind> {
    match name {
        "with_execution_policy" => Some(PolicyScopeKind::Execution),
        "with_approval_policy" => Some(PolicyScopeKind::ToolApproval),
        "with_command_policy" => Some(PolicyScopeKind::Command),
        "with_autonomy_policy" => Some(PolicyScopeKind::Autonomy),
        "with_dynamic_permissions" => Some(PolicyScopeKind::DynamicPermissions),
        _ => None,
    }
}

/// Collect the literal-arg vector for a synthesized harness CallSemantics
/// node. Mirrors the equivalent line in [`classify_call`] but avoids
/// re-running the classifier — the caller already supplies the ambient
/// name to dispatch through.
fn literal_args(args: &[SNode]) -> Vec<LiteralValue> {
    args.iter().map(literal_value).collect()
}

/// If `object` is the `harness.<sub_handle>` chain and `method` maps
/// to an ambient builtin via the per-sub-handle dispatch table, return
/// the sub-handle name and the ambient builtin to attribute the call
/// to. Returns `None` for arbitrary method calls so the existing
/// pass-through walk continues to handle them.
fn harness_sub_handle_for(object: &SNode, method: &str) -> Option<(&'static str, &'static str)> {
    let (sub_handle, root) = match &object.node {
        Node::PropertyAccess { object, property }
        | Node::OptionalPropertyAccess { object, property } => (property.as_str(), object.as_ref()),
        _ => return None,
    };
    let Node::Identifier(receiver) = &root.node else {
        return None;
    };
    if receiver != "harness" && receiver != "_harness" {
        return None;
    }
    HARNESS_SUB_HANDLES
        .iter()
        .find(|slug| **slug == sub_handle)
        .and_then(|slug| {
            harn_parser::harness_methods::harness_sub_handle_ambient(slug, method)
                .map(|ambient| (*slug, ambient))
        })
}

const HARNESS_SUB_HANDLES: &[&str] = &[
    "stdio", "term", "clock", "fs", "env", "random", "net", "process", "crypto", "system", "llm",
];

fn classify_call(name: &str, args: &[SNode]) -> CallSemantics {
    let literal_args = args.iter().map(literal_value).collect::<Vec<_>>();
    let mut display_name = name.to_string();
    let classification = match name {
        "request_approval" => CallClassification::ApprovalGate,
        "llm_budget_remaining" | "llm_budget" => CallClassification::BudgetRead,
        "egress_policy" => CallClassification::PolicyGate(PolicyScopeKind::Egress),
        "command_policy_push" => CallClassification::PolicyPush(PolicyScopeKind::Command),
        "command_policy_pop" => CallClassification::PolicyPop(PolicyScopeKind::Command),
        "write_file" | "write_file_bytes" | "append_file" | "delete_file" | "mkdir" | "mkdtemp"
        | "apply_edit" | "move_file" => {
            let path = literal_args
                .first()
                .and_then(LiteralValue::as_str)
                .map(str::to_string);
            capability_classification(vec![CapabilityEffect::new(
                Capability::WorkspaceMutation,
                name,
                path,
            )])
        }
        "copy_file" => {
            let path = literal_args
                .get(1)
                .and_then(LiteralValue::as_str)
                .map(str::to_string);
            capability_classification(vec![CapabilityEffect::new(
                Capability::WorkspaceMutation,
                name,
                path,
            )])
        }
        "exec" | "exec_at" | "shell" | "shell_at" | "spawn_captured" => {
            capability_classification(vec![CapabilityEffect::new(
                Capability::CommandExecution,
                name,
                None,
            )])
        }
        "mcp_call" => {
            let tool_name = literal_args
                .get(1)
                .and_then(LiteralValue::as_str)
                .map(str::to_string);
            if let Some(tool_name) = tool_name {
                display_name = tool_name.clone();
                classify_tool_call(&tool_name, literal_args.get(2))
            } else {
                capability_classification(vec![CapabilityEffect::new(
                    Capability::ConnectorAccess,
                    name,
                    None,
                )])
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
                capability_classification(vec![CapabilityEffect::new(
                    Capability::ConnectorAccess,
                    name,
                    None,
                )])
            }
        }
        "host_call" => classify_host_call(literal_args.first()),
        _ if is_model_call(name) => capability_classification(vec![CapabilityEffect::new(
            Capability::ModelCall,
            name,
            None,
        )]),
        _ if is_worker_dispatch(name) => capability_classification(vec![CapabilityEffect::new(
            Capability::WorkerDispatch,
            name,
            None,
        )]),
        _ if is_network_call(name) => capability_classification(vec![CapabilityEffect::new(
            Capability::NetworkAccess,
            name,
            None,
        )]),
        _ if name.starts_with("mcp_") => capability_classification(vec![CapabilityEffect::new(
            Capability::ConnectorAccess,
            name,
            None,
        )]),
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
    let mut effects = vec![CapabilityEffect::new(
        Capability::ConnectorAccess,
        tool_name,
        None,
    )];
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
        effects.push(CapabilityEffect::new(
            Capability::WorkspaceMutation,
            tool_name,
            path,
        ));
    }
    if normalized.contains("exec")
        || normalized.contains("shell")
        || normalized.contains("run")
        || normalized.contains("push_pr")
        || normalized.contains("create_pr")
        || normalized.contains("deploy")
    {
        effects.push(CapabilityEffect::new(
            Capability::CommandExecution,
            tool_name,
            None,
        ));
    }
    capability_classification(effects)
}

fn classify_host_call(name: Option<&LiteralValue>) -> CallClassification {
    let Some(operation) = name.and_then(LiteralValue::as_str) else {
        return capability_classification(vec![CapabilityEffect::new(
            Capability::ConnectorAccess,
            "host_call",
            None,
        )]);
    };
    if operation == "process.exec" || operation.starts_with("process.") {
        return capability_classification(vec![CapabilityEffect::new(
            Capability::CommandExecution,
            operation,
            None,
        )]);
    }
    if operation.starts_with("workspace.")
        && (operation.contains("write")
            || operation.contains("edit")
            || operation.contains("delete")
            || operation.contains("move")
            || operation.contains("patch"))
    {
        return capability_classification(vec![CapabilityEffect::new(
            Capability::WorkspaceMutation,
            operation,
            None,
        )]);
    }
    capability_classification(vec![CapabilityEffect::new(
        Capability::ConnectorAccess,
        operation,
        None,
    )])
}

fn capability_classification(effects: Vec<CapabilityEffect>) -> CallClassification {
    if effects.is_empty() {
        CallClassification::Other
    } else {
        CallClassification::Capabilities(effects)
    }
}

fn is_model_call(name: &str) -> bool {
    matches!(
        name,
        "llm_call"
            | "llm_call_safe"
            | "llm_stream_call"
            | "llm_call_structured"
            | "llm_call_structured_safe"
            | "llm_call_structured_result"
            | "llm_completion"
            | "agent_llm_turn"
            | "agent_turn"
            | "agent_loop"
    )
}

fn is_worker_dispatch(name: &str) -> bool {
    matches!(
        name,
        "spawn_agent"
            | "send_input"
            | "resume_agent"
            | "wait_agent"
            | "close_agent"
            | "worker_trigger"
            | "__host_sub_agent_run"
            | "__host_worker_spawn"
            | "__host_worker_send_input"
            | "__host_worker_resume"
            | "__host_worker_trigger"
            | "__host_worker_wait"
            | "__host_worker_close"
    )
}

fn is_network_call(name: &str) -> bool {
    matches!(
        name,
        "http_get"
            | "http_post"
            | "http_put"
            | "http_patch"
            | "http_delete"
            | "http_request"
            | "http_download"
            | "http_session"
            | "http_session_request"
            | "http_session_close"
            | "http_stream_open"
            | "http_stream_read"
            | "http_stream_close"
            | "sse_connect"
            | "sse_receive"
            | "sse_close"
            | "sse_server_response"
            | "sse_server_send"
            | "sse_server_heartbeat"
            | "sse_server_flush"
            | "sse_server_close"
            | "sse_server_cancel"
            | "websocket_accept"
            | "websocket_connect"
            | "websocket_send"
            | "websocket_receive"
            | "websocket_close"
            | "websocket_route"
            | "websocket_server"
            | "websocket_server_close"
            | "unix_socket_json_request"
            | "__net_unix_socket_json_request"
    )
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

use harn_glob::match_path as glob_match;

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

    fn handler_call_names(report: &AnalysisReport) -> Vec<String> {
        report
            .handlers
            .iter()
            .flat_map(|h| h.nodes.iter())
            .filter_map(|node| match &node.semantics {
                NodeSemantics::Call(call) => Some(call.name.clone()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn harness_fs_method_call_is_attributed_to_read_file() {
        let report = analyze(
            r#"
fn main(harness: Harness) {
  const body = harness.fs.read_text("notes.txt")
  harness.fs.mkdtemp("harn-ir-")
  harness.stdio.println(body)
}
"#,
        );

        let calls = handler_call_names(&report);
        assert!(
            calls.iter().any(|name| name == "read_file"),
            "expected harness.fs.read_text to lower to ambient read_file, got: {calls:?}"
        );
        assert!(
            calls.iter().any(|name| name == "mkdtemp"),
            "expected harness.fs.mkdtemp to lower to ambient mkdtemp, got: {calls:?}"
        );
        assert!(
            calls.iter().any(|name| name == "println"),
            "expected harness.stdio.println to lower to ambient println, got: {calls:?}"
        );
    }

    #[test]
    fn harness_net_method_call_is_attributed_to_http_get() {
        let report = analyze(
            r#"
fn main(harness: Harness) {
  harness.net.get("https://api.example.com")
}
"#,
        );

        let calls = handler_call_names(&report);
        assert!(
            calls.iter().any(|name| name == "http_get"),
            "expected harness.net.get to lower to ambient http_get, got: {calls:?}"
        );
    }

    #[test]
    fn harness_term_method_calls_are_attributed_to_terminal_builtins() {
        let report = analyze(
            r#"
fn main(harness: Harness) {
  harness.term.width()
  harness.term.height()
  harness.term.read_password("password: ")
}
"#,
        );

        let calls = handler_call_names(&report);
        assert!(
            calls.iter().any(|name| name == "term_width"),
            "expected harness.term.width to lower to ambient term_width, got: {calls:?}"
        );
        assert!(
            calls.iter().any(|name| name == "term_height"),
            "expected harness.term.height to lower to ambient term_height, got: {calls:?}"
        );
        assert!(
            calls.iter().any(|name| name == "read_password"),
            "expected harness.term.read_password to lower to read_password, got: {calls:?}"
        );
    }

    #[test]
    fn harness_process_method_call_is_attributed_to_spawn_captured() {
        let report = analyze(
            r#"
fn main(harness: Harness) {
  harness.process.spawn_captured({cmd: "printf", args: ["hi"]})
}
"#,
        );

        let calls = handler_call_names(&report);
        assert!(
            calls.iter().any(|name| name == "spawn_captured"),
            "expected harness.process.spawn_captured to lower to ambient spawn_captured, got: {calls:?}"
        );
    }

    #[test]
    fn harness_crypto_method_call_is_attributed_to_sha256_hex() {
        let report = analyze(
            r#"
fn main(harness: Harness) {
  harness.crypto.sha256("hello")
}
"#,
        );

        let calls = handler_call_names(&report);
        assert!(
            calls.iter().any(|name| name == "sha256_hex"),
            "expected harness.crypto.sha256 to lower to ambient sha256_hex, got: {calls:?}"
        );
    }

    #[test]
    fn harness_llm_method_calls_are_attributed_to_llm_catalog_builtins() {
        let report = analyze(
            r"
fn main(harness: Harness) {
  harness.llm.catalog()
  harness.llm.providers()
}
",
        );

        let calls = handler_call_names(&report);
        assert!(
            calls.iter().any(|name| name == "llm_catalog"),
            "expected harness.llm.catalog to lower to llm_catalog, got: {calls:?}"
        );
        assert!(
            calls.iter().any(|name| name == "llm_provider_status"),
            "expected harness.llm.providers to lower to llm_provider_status, got: {calls:?}"
        );
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
    fn capability_policy_rejects_undeclared_connector_access() {
        let report = analyze(
            r#"
@invariant("capability.policy", allow: "fs.write")
fn handler(client) {
  mcp_call(client, "github.search", {})
}
"#,
        );

        let diags = diagnostics_by_invariant(&report, "capability.policy");
        assert_eq!(diags.len(), 1);
        assert!(diags[0].message.contains("mcp.connector"));
        assert!(diags[0].message.contains("not declared"));
        assert_eq!(diags[0].handler, "handler");
        assert!(diags[0]
            .path
            .iter()
            .any(|step| step.label.contains("github.search")));
    }

    #[test]
    fn capability_policy_rejects_workspace_mutation_outside_allowed_glob() {
        let report = analyze(
            r#"
@invariant("capability.policy", allow: "fs.write", workspace: "src/**")
fn handler() {
  write_file("/tmp/out.txt", "unsafe")
}
"#,
        );

        let diags = diagnostics_by_invariant(&report, "capability.policy");
        assert_eq!(diags.len(), 1);
        assert!(diags[0]
            .message
            .contains("outside the allowed workspace glob"));
    }

    #[test]
    fn capability_policy_accepts_approved_workspace_mutation_and_budgeted_llm() {
        let report = analyze(
            r#"
@invariant("capability.policy",
  allow: "fs.write,llm.model",
  workspace: "src/**",
  require_approval: "fs.write",
  require_budget: "llm.model")
fn handler() {
  request_approval("edit", {capabilities_requested: ["fs.write"]})
  write_file("src/main.rs", "safe")
  llm_call("summarize", nil, {budget: {max_output_tokens: 64}})
}
"#,
        );

        assert!(
            diagnostics_by_invariant(&report, "capability.policy").is_empty(),
            "unexpected diagnostics: {:?}",
            report.diagnostics
        );
    }

    #[test]
    fn capability_policy_requires_command_policy_for_exec() {
        let report = analyze(
            r#"
@invariant("capability.policy",
  allow: "process.exec",
  require_command_policy: "process.exec")
fn handler() {
  exec("rm -rf /tmp/harn")
}
"#,
        );

        let diags = diagnostics_by_invariant(&report, "capability.policy");
        assert_eq!(diags.len(), 1);
        assert!(diags[0].message.contains("process.exec"));
        assert!(diags[0].message.contains("command policy"));

        let report = analyze(
            r#"
@invariant("capability.policy",
  allow: "process.exec",
  require_command_policy: "process.exec")
fn handler() {
  with_command_policy({deny: ["rm"]}, { ->
    exec("echo ok")
  })
}
"#,
        );

        assert!(
            diagnostics_by_invariant(&report, "capability.policy").is_empty(),
            "unexpected diagnostics: {:?}",
            report.diagnostics
        );
    }

    #[test]
    fn capability_policy_tracks_command_policy_push_and_pop() {
        let report = analyze(
            r#"
@invariant("capability.policy",
  allow: "process.exec",
  require_command_policy: "process.exec")
fn handler() {
  command_policy_push({deny: ["rm"]})
  exec("echo ok")
  command_policy_pop()
}
"#,
        );

        assert!(
            diagnostics_by_invariant(&report, "capability.policy").is_empty(),
            "unexpected diagnostics: {:?}",
            report.diagnostics
        );

        let report = analyze(
            r#"
@invariant("capability.policy",
  allow: "process.exec",
  require_command_policy: "process.exec")
fn handler() {
  command_policy_push({deny: ["rm"]})
  command_policy_pop()
  exec("echo unsafe")
}
"#,
        );

        let diags = diagnostics_by_invariant(&report, "capability.policy");
        assert_eq!(diags.len(), 1);
        assert!(diags[0].message.contains("command policy"));
    }

    #[test]
    fn capability_policy_requires_egress_policy_for_network_and_connector_access() {
        let report = analyze(
            r#"
@invariant("capability.policy",
  allow: "network.access,mcp.connector",
  require_egress_policy: "network.access,mcp.connector")
fn handler(client) {
  http_request("https://example.com")
  mcp_call(client, "github.search", {})
}
"#,
        );

        let diags = diagnostics_by_invariant(&report, "capability.policy");
        assert_eq!(diags.len(), 2);
        assert!(diags
            .iter()
            .any(|diag| diag.message.contains("network.access")));
        assert!(diags
            .iter()
            .any(|diag| diag.message.contains("mcp.connector")));

        let report = analyze(
            r#"
@invariant("capability.policy",
  allow: "network.access,mcp.connector",
  require_egress_policy: "network.access,mcp.connector")
fn handler(client) {
  egress_policy({default: "deny", allow: ["example.com"]})
  http_request("https://example.com")
  mcp_call(client, "github.search", {})
}
"#,
        );

        assert!(
            diagnostics_by_invariant(&report, "capability.policy").is_empty(),
            "unexpected diagnostics: {:?}",
            report.diagnostics
        );
    }

    #[test]
    fn capability_policy_treats_unix_socket_json_request_as_network_access() {
        let report = analyze(
            r#"
@invariant("capability.policy",
  allow: "network.access",
  require_egress_policy: "network.access")
fn handler() {
  unix_socket_json_request("/tmp/harn.sock", {})
}
"#,
        );

        let diags = diagnostics_by_invariant(&report, "capability.policy");
        assert_eq!(diags.len(), 1);
        assert!(diags[0].message.contains("network.access"));
    }

    #[test]
    fn capability_policy_requires_autonomy_policy_for_worker_dispatch() {
        let report = analyze(
            r#"
@invariant("capability.policy",
  allow: "worker.dispatch",
  require_autonomy: "worker.dispatch")
fn handler() {
  spawn_agent({task: "summarize"})
}
"#,
        );

        let diags = diagnostics_by_invariant(&report, "capability.policy");
        assert_eq!(diags.len(), 1);
        assert!(diags[0].message.contains("worker.dispatch"));
        assert!(diags[0].message.contains("autonomy policy"));

        let report = analyze(
            r#"
@invariant("capability.policy",
  allow: "worker.dispatch",
  require_autonomy: "worker.dispatch")
fn handler() {
  with_autonomy_policy({autonomy_tier: "act_with_approval"}, { ->
    spawn_agent({task: "summarize"})
  })
}
"#,
        );

        assert!(
            diagnostics_by_invariant(&report, "capability.policy").is_empty(),
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
        // `**/` also matches zero directories (git/globset convention).
        assert!(glob_match("src/**/*.rs", "src/main.rs"));
    }
}
