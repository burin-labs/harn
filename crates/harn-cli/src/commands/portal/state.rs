use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use tokio::sync::Mutex;

use super::dto::PortalLaunchJob;
use harn_vm::event_log::AnyEventLog;

#[derive(Clone)]
pub(super) struct PortalState {
    pub(super) run_dir: PathBuf,
    pub(super) workspace_root: PathBuf,
    pub(super) event_log: Option<Arc<AnyEventLog>>,
    pub(super) launch_program: PathBuf,
    pub(super) launch_jobs: Arc<Mutex<HashMap<String, PortalLaunchJob>>>,
}
