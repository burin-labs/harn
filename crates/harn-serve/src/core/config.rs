use std::num::NonZeroUsize;
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
    /// Maximum number of isolated VM executions that explicitly safe exports
    /// may run at once. Unknown or mutating exports remain globally ordered.
    pub max_dispatch_workers: NonZeroUsize,
}

impl DispatchCoreConfig {
    pub fn for_script(path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        // Dispatch changes the VM source directory to the script's parent. An
        // absolute identity prevents a relative `dir/script.harn` from being
        // resolved as `dir/dir/script.harn` when an exported call loads it.
        let script_path = if path.is_absolute() {
            path
        } else {
            std::env::current_dir()
                .unwrap_or_else(|_| PathBuf::from("."))
                .join(path)
        };
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
            max_dispatch_workers: NonZeroUsize::new(
                std::thread::available_parallelism()
                    .map(NonZeroUsize::get)
                    .unwrap_or(1)
                    .min(4),
            )
            .expect("dispatch worker ceiling is non-zero"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relative_script_paths_are_bound_to_the_launch_directory() {
        let relative = PathBuf::from("examples/server.harn");
        let config = DispatchCoreConfig::for_script(&relative);
        assert!(config.script_path.is_absolute());
        assert!(config.script_path.ends_with(&relative));
        assert_eq!(config.base_dir, config.script_path.parent().unwrap());
    }
}
