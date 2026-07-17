use std::sync::Arc;

use crate::trust_graph::AutonomyTier;
use crate::value::VmClosure;

#[derive(Clone)]
pub enum TriggerHandlerSpec {
    Local {
        raw: String,
        callable: crate::value::VmCallable,
    },
    A2a {
        target: String,
        allow_cleartext: bool,
    },
    Worker {
        queue: String,
    },
    Persona {
        binding: crate::PersonaRuntimeBinding,
        callable: crate::value::VmCallable,
    },
    EvalPack {
        target: String,
        manifest: Box<crate::orchestration::EvalPackManifest>,
        ledger_options: Option<serde_json::Value>,
    },
    AutoResume {
        worker_id: String,
    },
    /// Composes triggers (#1870) with named agent pools (#1883). On match,
    /// the dispatcher resolves `pool` by name, invokes `task_factory(event)`
    /// to build the per-event closure, and submits it to the pool with the
    /// pool's queue strategy + backpressure policy applied (PL-04 / #1889).
    SpawnToPool {
        pool: String,
        priority_from: Option<String>,
        key_from: Option<String>,
        task_factory: Arc<VmClosure>,
    },
    /// Composes triggers (#1870) with the reminder pipeline (#1815) by
    /// injecting a `SystemReminder` into a target agent session's transcript
    /// when the trigger matches (CH-05 / #1876). The reminder appears at the
    /// session's next turn boundary — same as any other reminder. No spawn,
    /// no resume, no signal. `body` is a prompt-template string rendered
    /// against `{event}` / `{batch.events}` per match. `target` resolution
    /// happens at dispatch time via [`TargetExpr`]; missing-target dispatches
    /// are recorded as audit events rather than crashing the trigger.
    ReminderInject {
        target: TargetExpr,
        body: String,
        tags: Vec<String>,
        ttl_turns: Option<i64>,
        dedupe_key: Option<String>,
        propagate: crate::llm::helpers::ReminderPropagate,
        role_hint: crate::llm::helpers::ReminderRoleHint,
        preserve_on_compact: bool,
    },
    /// CH-10 (#1910): emergency "panic" broadcast. When the trigger fires,
    /// every running worker matched by `target_agents` is suspended
    /// synchronously via the cooperative suspend pipeline used by
    /// `suspend_agent` (#1837), bypassing the normal turn-boundary
    /// delivery contract. Already-suspended or terminal workers are skipped
    /// (no double-suspend, no error). `reason` is propagated to the
    /// suspension envelope, the `WorkerSuspended` event, and the
    /// `triggers.interrupt_and_suspend.audit` audit entry per suspension.
    /// A no-target dispatch records a single roll-up audit entry and
    /// returns a `broadcast` result with `suspended_count: 0` — graceful
    /// no-op rather than dispatch failure, mirroring the `ReminderInject`
    /// missing-target contract.
    InterruptAndSuspend {
        target_agents: AgentScope,
        reason: String,
    },
}

/// Resolution mode for the target worker set of a
/// [`TriggerHandlerSpec::InterruptAndSuspend`] dispatch. `All` enumerates
/// every entry in the local worker registry (the org-scoped "panic"
/// broadcast); `Concrete` carries an explicit worker-id list from the
/// registration; `Closure` defers resolution to a user closure that runs at
/// dispatch time with the event payload bound to its first argument and
/// must return a list of worker-id strings.
#[derive(Clone)]
pub enum AgentScope {
    All,
    Concrete(Vec<String>),
    Closure(Arc<VmClosure>),
}

impl AgentScope {
    pub fn kind(&self) -> &'static str {
        match self {
            Self::All => "all",
            Self::Concrete(_) => "concrete",
            Self::Closure(_) => "closure",
        }
    }
}

impl std::fmt::Debug for AgentScope {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::All => f.write_str("All"),
            Self::Concrete(ids) => f.debug_tuple("Concrete").field(ids).finish(),
            Self::Closure(_) => f.write_str("Closure(<vm_closure>)"),
        }
    }
}

/// Resolution mode for the target session of a [`TriggerHandlerSpec::ReminderInject`]
/// dispatch. `Current` walks the current-session thread-local; `Parent` resolves
/// the active session's parent in the agent-session lineage; `Concrete` carries
/// a literal session id from the registration; `Closure` defers resolution to
/// a user closure that runs at dispatch time with the event payload bound to
/// its first argument and must return the session id as a string.
#[derive(Clone)]
pub enum TargetExpr {
    Current,
    Parent,
    Concrete(String),
    Closure(Arc<VmClosure>),
}

impl TargetExpr {
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Current => "current",
            Self::Parent => "parent",
            Self::Concrete(_) => "concrete",
            Self::Closure(_) => "closure",
        }
    }
}

impl std::fmt::Debug for TargetExpr {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Current => f.write_str("Current"),
            Self::Parent => f.write_str("Parent"),
            Self::Concrete(id) => f.debug_tuple("Concrete").field(id).finish(),
            Self::Closure(_) => f.write_str("Closure(<vm_closure>)"),
        }
    }
}

impl std::fmt::Debug for TriggerHandlerSpec {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Local { raw, .. } => f.debug_struct("Local").field("raw", raw).finish(),
            Self::A2a {
                target,
                allow_cleartext,
            } => f
                .debug_struct("A2a")
                .field("target", target)
                .field("allow_cleartext", allow_cleartext)
                .finish(),
            Self::Worker { queue } => f.debug_struct("Worker").field("queue", queue).finish(),
            Self::Persona { binding, .. } => f
                .debug_struct("Persona")
                .field("name", &binding.name)
                .finish(),
            Self::EvalPack {
                target,
                manifest,
                ledger_options,
            } => f
                .debug_struct("EvalPack")
                .field("target", target)
                .field("pack_id", &manifest.id)
                .field("ledger_options", ledger_options)
                .finish(),
            Self::AutoResume { worker_id } => f
                .debug_struct("AutoResume")
                .field("worker_id", worker_id)
                .finish(),
            Self::SpawnToPool {
                pool,
                priority_from,
                key_from,
                ..
            } => f
                .debug_struct("SpawnToPool")
                .field("pool", pool)
                .field("priority_from", priority_from)
                .field("key_from", key_from)
                .finish(),
            Self::ReminderInject {
                target,
                body,
                tags,
                ttl_turns,
                dedupe_key,
                propagate,
                role_hint,
                preserve_on_compact,
            } => f
                .debug_struct("ReminderInject")
                .field("target", target)
                .field("body", body)
                .field("tags", tags)
                .field("ttl_turns", ttl_turns)
                .field("dedupe_key", dedupe_key)
                .field("propagate", propagate)
                .field("role_hint", role_hint)
                .field("preserve_on_compact", preserve_on_compact)
                .finish(),
            Self::InterruptAndSuspend {
                target_agents,
                reason,
            } => f
                .debug_struct("InterruptAndSuspend")
                .field("target_agents", target_agents)
                .field("reason", reason)
                .finish(),
        }
    }
}

impl TriggerHandlerSpec {
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Local { .. } => "local",
            Self::A2a { .. } => "a2a",
            Self::Worker { .. } => "worker",
            Self::Persona { .. } => "persona",
            Self::EvalPack { .. } => "eval_pack",
            Self::AutoResume { .. } => "auto_resume",
            Self::SpawnToPool { .. } => "spawn_to_pool",
            Self::ReminderInject { .. } => "reminder_inject",
            Self::InterruptAndSuspend { .. } => "interrupt_and_suspend",
        }
    }

    pub fn effective_autonomy_tier(&self, requested: AutonomyTier) -> AutonomyTier {
        match self {
            Self::Local { callable, .. } | Self::Persona { callable, .. } => {
                callable.effective_autonomy_tier(requested)
            }
            _ => requested,
        }
    }
}
