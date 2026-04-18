use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize)]
pub(super) struct RunQuery {
    pub(super) path: String,
}

#[derive(Debug, Deserialize)]
pub(super) struct CompareQuery {
    pub(super) left: String,
    pub(super) right: String,
}

#[derive(Debug, Deserialize)]
pub(super) struct ListRunsQuery {
    pub(super) q: Option<String>,
    pub(super) workflow: Option<String>,
    pub(super) status: Option<String>,
    pub(super) sort: Option<String>,
    pub(super) page: Option<usize>,
    pub(super) page_size: Option<usize>,
    /// Only include runs where the named skill was activated at any
    /// point. Drives the portal's "runs where skill X was active"
    /// eval-selection filter (see harn#76 acceptance criteria).
    pub(super) skill: Option<String>,
}

#[derive(Debug, Serialize)]
pub(super) struct ErrorResponse {
    pub(super) error: String,
}
