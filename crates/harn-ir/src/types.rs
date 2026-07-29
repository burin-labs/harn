//! The IR data model.
//!
//! Handlers, nodes and edges, the semantics attached to a node, and the
//! capability vocabulary the invariants reason over. `AnalysisReport` is what
//! `analyze_program` hands back; `HandlerIr` is one handler's graph.

use harn_lexer::Span;
use harn_parser::SNode;
use std::collections::BTreeMap;

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
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Self::String(value) | Self::Identifier(value) => Some(value.as_str()),
            _ => None,
        }
    }

    pub fn dict_field(&self, key: &str) -> Option<&LiteralValue> {
        match self {
            Self::Dict(entries) => entries.get(key),
            _ => None,
        }
    }

    pub fn list_items(&self) -> Option<&[LiteralValue]> {
        match self {
            Self::List(items) => Some(items),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Capability {
    FilesystemRead,
    WorkspaceMutation,
    CommandExecution,
    NetworkAccess,
    ConnectorAccess,
    ModelCall,
    WorkerDispatch,
    Stdio,
    Environment,
    Clock,
    Random,
    Secret,
    Observability,
    Channel,
    State,
    HumanApproval,
    AutonomyPolicy,
}

impl Capability {
    /// Stable policy/graph label for this capability family.
    pub const fn canonical(self) -> &'static str {
        match self {
            Self::FilesystemRead => "fs.read",
            Self::WorkspaceMutation => "fs.write",
            Self::CommandExecution => "process.exec",
            Self::NetworkAccess => "network.access",
            Self::ConnectorAccess => "mcp.connector",
            Self::ModelCall => "llm.model",
            Self::WorkerDispatch => "worker.dispatch",
            Self::Stdio => "stdio.access",
            Self::Environment => "env.read",
            Self::Clock => "clock.access",
            Self::Random => "random.read",
            Self::Secret => "secret.access",
            Self::Observability => "observability.emit",
            Self::Channel => "channel.access",
            Self::State => "state.access",
            Self::HumanApproval => "human.approval",
            Self::AutonomyPolicy => "autonomy.policy",
        }
    }

    pub(crate) fn from_policy_name(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "fs.read" | "filesystem.read" => Some(Self::FilesystemRead),
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
            "stdio.access" | "stdio" => Some(Self::Stdio),
            "env.read" | "env" | "environment" => Some(Self::Environment),
            "clock.access" | "clock" => Some(Self::Clock),
            "random.read" | "random" => Some(Self::Random),
            "secret.access" | "secret" | "secrets" => Some(Self::Secret),
            "observability.emit" | "observability" => Some(Self::Observability),
            "channel.access" | "channel" | "channels" => Some(Self::Channel),
            "state.access" | "state" => Some(Self::State),
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
    pub access: harn_builtin_meta::EffectAccess,
}

impl CapabilityEffect {
    pub(crate) fn from_contract(
        capability: Capability,
        operation: impl Into<String>,
        path: Option<String>,
        access: harn_builtin_meta::EffectAccess,
    ) -> Self {
        Self {
            capability,
            operation: operation.into(),
            path,
            access,
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
    pub(crate) fn label(self) -> &'static str {
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
    pub(crate) fn capability_effects(&self) -> &[CapabilityEffect] {
        match &self.classification {
            CallClassification::Capabilities(effects) => effects,
            _ => &[],
        }
    }

    pub(crate) fn has_budget_option(&self) -> bool {
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
