use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use tokio::sync::Mutex;

use super::dto::PortalLaunchJob;

#[derive(Clone)]
pub(super) struct PortalState {
    pub(super) run_dir: PathBuf,
    pub(super) workspace_root: PathBuf,
    pub(super) launch_program: PathBuf,
    pub(super) launch_jobs: Arc<Mutex<HashMap<String, PortalLaunchJob>>>,
}
