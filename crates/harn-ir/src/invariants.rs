//! The invariants checked against a handler's IR.
//!
//! `Invariant` is the trait each check implements: filesystem writes staying
//! inside the declared globs, budget remaining never increasing, approval
//! reachability, and the capability policy — which tracks scope depth so a
//! nested policy cannot widen the one enclosing it.

use std::collections::{BTreeSet, HashSet, VecDeque};

use crate::classify::*;
use crate::types::*;
use harn_glob::match_path as glob_match;
pub trait Invariant {
    fn name(&self) -> &'static str;
    fn check(&self, ir: &HandlerIr) -> Vec<InvariantDiagnostic>;
}

#[derive(Debug, Clone)]
pub struct FsWritesSubsetPathGlob {
    pub(crate) globs: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct BudgetRemainingNonIncreasing {
    pub(crate) target: String,
}

#[derive(Debug, Clone, Default)]
pub struct ApprovalReachability;

#[derive(Debug, Clone)]
pub struct CapabilityPolicyInvariant {
    pub(crate) allowed: BTreeSet<Capability>,
    pub(crate) workspace_globs: Vec<String>,
    pub(crate) require_approval: BTreeSet<Capability>,
    pub(crate) require_budget: BTreeSet<Capability>,
    pub(crate) require_autonomy: BTreeSet<Capability>,
    pub(crate) require_execution_policy: BTreeSet<Capability>,
    pub(crate) require_command_policy: BTreeSet<Capability>,
    pub(crate) require_egress_policy: BTreeSet<Capability>,
    pub(crate) require_approval_policy: BTreeSet<Capability>,
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
                "install `harness.net.egress_policy(...)` before the reachable network or connector call",
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
