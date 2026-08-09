use std::path::{Path, PathBuf};
use std::sync::Arc;

use harn_vm::trust_graph::AutonomyTier;

use super::VmConfigurator;
use super::{AuthPolicy, InMemoryReplayCache, LimitRegistry, NoopVmConfigurator, ReplayCache};

pub struct DispatchCoreConfig {
    pub script_path: PathBuf,
    pub base_dir: PathBuf,
    pub service_name: String,
    pub autonomy_tier: AutonomyTier,
    pub auth_policy: AuthPolicy,
    pub replay_cache: Arc<dyn ReplayCache>,
    pub vm_configurator: Arc<dyn VmConfigurator>,
    /// Grant the embedder-selected route module graph access to privileged
    /// host builtins. Ordinary Harn imports remain unprivileged.
    pub trusted_host_dispatch: bool,
    /// Rate-limit + backpressure orchestrator. `None` short-circuits
    /// the limits check (every dispatch admitted unconditionally),
    /// matching legacy `harn-serve` behaviour. Production deployments
    /// install [`LimitRegistry::in_memory`] (single-node default) or a
    /// cluster-aware impl that wraps a remote counter.
    pub limit_registry: Option<Arc<LimitRegistry>>,
}

impl DispatchCoreConfig {
    pub fn for_script(path: impl Into<PathBuf>) -> Self {
        let script_path = path.into();
        let base_dir = script_path.parent().unwrap_or(Path::new(".")).to_path_buf();
        let service_name = script_path
            .file_stem()
            .and_then(|value| value.to_str())
            .unwrap_or("harn-serve")
            .to_string();
        Self {
            script_path,
            base_dir,
            service_name,
            autonomy_tier: AutonomyTier::ActAuto,
            auth_policy: AuthPolicy::allow_all(),
            replay_cache: Arc::new(InMemoryReplayCache::new()),
            vm_configurator: Arc::new(NoopVmConfigurator),
            trusted_host_dispatch: false,
            limit_registry: None,
        }
    }
}
