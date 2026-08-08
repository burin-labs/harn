//! Typed collaborative plan documents and their deterministic event replay.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::PLAN_SCHEMA_VERSION;

pub const PLAN_DOCUMENT_SCHEMA_VERSION: &str = "harn.plan_document.v1";
pub const PLAN_DOCUMENT_ARTIFACT_KIND: &str = "plan_document";
pub const PLAN_DOCUMENT_SCHEMA_ARTIFACT: &str = "schemas/plan-document-v1.schema.json";

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PlanStepStatus {
    Pending,
    InProgress,
    Completed,
    Blocked,
    Cancelled,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct PlanStep {
    pub id: String,
    pub content: String,
    pub status: PlanStepStatus,
    pub priority: Option<serde_json::Value>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PlanApprovalState {
    Unrequested,
    Requested,
    Approved,
    Rejected,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct PlanApproval {
    pub state: PlanApprovalState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reviewer: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reviewers: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approved_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct PlanArtifact {
    #[serde(rename = "_type")]
    pub type_name: String,
    pub schema_version: String,
    pub id: String,
    pub tool: String,
    pub title: String,
    pub summary: String,
    pub steps: Vec<PlanStep>,
    pub assumptions: Vec<String>,
    pub open_questions: Vec<String>,
    pub verification_commands: Vec<String>,
    pub approval: PlanApproval,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct PlanAuthor {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct PlanSource {
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uri: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct PlanRevision {
    pub revision_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_revision_id: Option<String>,
    pub markdown: String,
    pub plan: PlanArtifact,
    pub author: PlanAuthor,
    pub source: PlanSource,
    pub created_at: String,
    pub operation: PlanRevisionOperation,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PlanRevisionOperation {
    Create {
        event_id: String,
    },
    Edit {
        event_id: String,
    },
    Comment {
        event_id: String,
        comment_id: String,
    },
    CommentState {
        event_id: String,
        comment_id: String,
        state: PlanCommentState,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct PlanTextRange {
    pub start: usize,
    pub end: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct PlanCommentAnchor {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub step_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quoted_text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub range: Option<PlanTextRange>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PlanCommentState {
    Open,
    Addressed,
    Resolved,
    Reopened,
}

impl PlanCommentState {
    pub fn is_unresolved(&self) -> bool {
        !matches!(self, Self::Resolved)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct PlanComment {
    pub comment_id: String,
    pub anchor: PlanCommentAnchor,
    pub body: String,
    pub state: PlanCommentState,
    pub author: PlanAuthor,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct PlanCommentResolutionReceipt {
    pub receipt_id: String,
    pub comment_id: String,
    pub input_revision_id: String,
    pub output_revision_id: String,
    pub agent_run_id: String,
    pub event_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub explanation: Option<String>,
    pub created_at: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct PlanDocument {
    #[serde(rename = "_type")]
    pub type_name: String,
    pub schema_version: String,
    pub document_id: String,
    pub current_revision: PlanRevision,
    pub comments: Vec<PlanComment>,
    pub resolution_receipts: Vec<PlanCommentResolutionReceipt>,
    pub created_at: String,
    pub updated_at: String,
}

impl PlanDocument {
    pub fn unresolved_comments(&self) -> impl Iterator<Item = &PlanComment> {
        self.comments
            .iter()
            .filter(|comment| comment.state.is_unresolved())
    }

    pub fn validate(&self) -> Result<(), PlanDocumentError> {
        require(
            self.type_name == PLAN_DOCUMENT_ARTIFACT_KIND,
            "document _type must be plan_document",
        )?;
        require(
            self.schema_version == PLAN_DOCUMENT_SCHEMA_VERSION,
            "unsupported plan document schema_version",
        )?;
        require(
            !self.document_id.trim().is_empty(),
            "document_id is required",
        )?;
        require(
            !self.created_at.trim().is_empty(),
            "document created_at is required",
        )?;
        require(
            self.updated_at == self.current_revision.created_at,
            "document updated_at must match the current revision",
        )?;
        validate_revision(&self.current_revision)?;

        let mut comment_ids = BTreeSet::new();
        for comment in &self.comments {
            require(
                comment_ids.insert(comment.comment_id.as_str()),
                "comment_id values must be unique",
            )?;
            validate_comment(comment, &self.current_revision)?;
        }
        let comments = self
            .comments
            .iter()
            .map(|comment| comment.comment_id.as_str())
            .collect::<BTreeSet<_>>();
        let mut receipt_ids = BTreeSet::new();
        for receipt in &self.resolution_receipts {
            require(
                receipt_ids.insert(receipt.receipt_id.as_str()),
                "receipt_id values must be unique",
            )?;
            require(
                comments.contains(receipt.comment_id.as_str()),
                "resolution receipt references an unknown comment",
            )?;
            for value in [
                &receipt.input_revision_id,
                &receipt.output_revision_id,
                &receipt.agent_run_id,
                &receipt.event_id,
                &receipt.created_at,
            ] {
                require(
                    !value.trim().is_empty(),
                    "resolution receipt fields are required",
                )?;
            }
            require(
                receipt.receipt_id == resolution_receipt_id(receipt)?,
                "receipt_id does not match immutable resolution receipt state",
            )?;
        }
        validate_revision_identity(
            &self.current_revision,
            &self.comments,
            &self.resolution_receipts,
        )?;
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PlanDocumentEvent {
    Created {
        event_id: String,
        document: PlanDocument,
    },
    Updated {
        event_id: String,
        input_revision_id: String,
        document: PlanDocument,
    },
}

impl PlanDocumentEvent {
    pub fn document(&self) -> &PlanDocument {
        match self {
            Self::Created { document, .. } | Self::Updated { document, .. } => document,
        }
    }

    pub fn event_id(&self) -> &str {
        match self {
            Self::Created { event_id, .. } | Self::Updated { event_id, .. } => event_id,
        }
    }

    pub fn to_artifact_record(
        &self,
    ) -> Result<crate::orchestration::ArtifactRecord, PlanDocumentError> {
        let document = self.document();
        document.validate()?;
        let mut metadata = BTreeMap::new();
        metadata.insert(
            "schema_version".to_string(),
            serde_json::Value::String(PLAN_DOCUMENT_SCHEMA_VERSION.to_string()),
        );
        metadata.insert(
            "document_id".to_string(),
            serde_json::Value::String(document.document_id.clone()),
        );
        metadata.insert(
            "revision_id".to_string(),
            serde_json::Value::String(document.current_revision.revision_id.clone()),
        );
        Ok(crate::orchestration::ArtifactRecord {
            type_name: "artifact".to_string(),
            id: format!(
                "plan_document_event_{}",
                self.event_id().trim_start_matches("plan_event_")
            ),
            kind: PLAN_DOCUMENT_ARTIFACT_KIND.to_string(),
            title: Some(document.current_revision.plan.title.clone()),
            text: Some(document.current_revision.markdown.clone()),
            data: Some(serde_json::to_value(self).map_err(|error| {
                PlanDocumentError::Invalid(format!("cannot persist plan document event: {error}"))
            })?),
            source: Some(document.current_revision.source.kind.clone()),
            created_at: document.updated_at.clone(),
            freshness: Some("fresh".to_string()),
            priority: Some(80),
            lineage: document
                .current_revision
                .parent_revision_id
                .iter()
                .cloned()
                .collect(),
            relevance: None,
            estimated_tokens: None,
            stage: Some("plan".to_string()),
            metadata,
        }
        .normalize())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum PlanDocumentError {
    #[error(
        "plan document {document_id} revision conflict: expected {expected_revision_id}, current revision is {current_revision_id}"
    )]
    Conflict {
        document_id: String,
        expected_revision_id: String,
        current_revision_id: String,
    },
    #[error("invalid collaborative plan document: {0}")]
    Invalid(String),
    #[error("plan comment {0} was not found")]
    CommentNotFound(String),
    #[error("invalid plan comment transition from {from:?} to {to:?}")]
    InvalidCommentTransition {
        from: PlanCommentState,
        to: PlanCommentState,
    },
    #[error("plan document replay failed at event {event_id}: {message}")]
    Replay { event_id: String, message: String },
}

#[derive(Clone, Debug)]
pub struct CreatePlanDocument {
    pub document_id: String,
    pub markdown: String,
    pub plan: PlanArtifact,
    pub author: PlanAuthor,
    pub source: PlanSource,
    pub created_at: String,
    pub event_id: String,
}

#[derive(Clone, Debug)]
pub struct EditPlanDocument {
    pub expected_revision_id: String,
    pub markdown: String,
    pub plan: PlanArtifact,
    pub author: PlanAuthor,
    pub source: PlanSource,
    pub created_at: String,
    pub event_id: String,
}

#[derive(Clone, Debug)]
pub struct AddPlanComment {
    pub expected_revision_id: String,
    pub comment_id: String,
    pub anchor: PlanCommentAnchor,
    pub body: String,
    pub author: PlanAuthor,
    pub created_at: String,
    pub event_id: String,
}

#[derive(Clone, Debug)]
pub struct ChangePlanCommentState {
    pub expected_revision_id: String,
    pub comment_id: String,
    pub state: PlanCommentState,
    pub author: PlanAuthor,
    pub source: PlanSource,
    pub created_at: String,
    pub event_id: String,
    pub agent_run_id: Option<String>,
    pub explanation: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlanDocumentStore {
    document: PlanDocument,
    events: Vec<PlanDocumentEvent>,
}

impl PlanDocumentStore {
    pub fn create(input: CreatePlanDocument) -> Result<Self, PlanDocumentError> {
        require(
            !input.document_id.trim().is_empty(),
            "document_id is required",
        )?;
        validate_plan(&input.plan)?;
        let revision = make_revision(
            None,
            input.markdown,
            input.plan,
            input.author,
            input.source,
            input.created_at.clone(),
            PlanRevisionOperation::Create {
                event_id: input.event_id.clone(),
            },
            RevisionState {
                comments: &[],
                receipts: &[],
            },
        )?;
        let document = PlanDocument {
            type_name: PLAN_DOCUMENT_ARTIFACT_KIND.to_string(),
            schema_version: PLAN_DOCUMENT_SCHEMA_VERSION.to_string(),
            document_id: input.document_id,
            current_revision: revision,
            comments: Vec::new(),
            resolution_receipts: Vec::new(),
            created_at: input.created_at.clone(),
            updated_at: input.created_at,
        };
        document.validate()?;
        let event = PlanDocumentEvent::Created {
            event_id: input.event_id,
            document: document.clone(),
        };
        Ok(Self {
            document,
            events: vec![event],
        })
    }

    pub fn current(&self) -> &PlanDocument {
        &self.document
    }

    pub fn resume(document: PlanDocument) -> Result<Self, PlanDocumentError> {
        document.validate()?;
        Ok(Self {
            document,
            events: Vec::new(),
        })
    }

    pub fn events(&self) -> &[PlanDocumentEvent] {
        &self.events
    }

    pub fn edit(&mut self, input: EditPlanDocument) -> Result<&PlanDocument, PlanDocumentError> {
        self.require_revision(&input.expected_revision_id)?;
        validate_plan(&input.plan)?;
        let parent = self.document.current_revision.revision_id.clone();
        let revision = make_revision(
            Some(parent.clone()),
            input.markdown,
            input.plan,
            input.author,
            input.source,
            input.created_at.clone(),
            PlanRevisionOperation::Edit {
                event_id: input.event_id.clone(),
            },
            RevisionState {
                comments: &self.document.comments,
                receipts: &self.document.resolution_receipts,
            },
        )?;
        self.document.current_revision = revision;
        self.document.updated_at = input.created_at;
        self.commit(input.event_id, parent)
    }

    pub fn add_comment(
        &mut self,
        input: AddPlanComment,
    ) -> Result<&PlanDocument, PlanDocumentError> {
        self.require_revision(&input.expected_revision_id)?;
        require(
            !self
                .document
                .comments
                .iter()
                .any(|comment| comment.comment_id == input.comment_id),
            "comment_id values must be unique",
        )?;
        let comment = PlanComment {
            comment_id: input.comment_id.clone(),
            anchor: input.anchor,
            body: input.body,
            state: PlanCommentState::Open,
            author: input.author.clone(),
            created_at: input.created_at.clone(),
            updated_at: input.created_at.clone(),
        };
        validate_comment(&comment, &self.document.current_revision)?;
        let parent = self.document.current_revision.revision_id.clone();
        self.document.comments.push(comment);
        self.revise_unchanged_content(
            input.author,
            PlanSource {
                kind: "comment".to_string(),
                uri: None,
            },
            input.created_at,
            PlanRevisionOperation::Comment {
                event_id: input.event_id.clone(),
                comment_id: input.comment_id,
            },
        )?;
        self.commit(input.event_id, parent)
    }

    pub fn change_comment_state(
        &mut self,
        input: ChangePlanCommentState,
    ) -> Result<&PlanDocument, PlanDocumentError> {
        self.require_revision(&input.expected_revision_id)?;
        let index = self
            .document
            .comments
            .iter()
            .position(|comment| comment.comment_id == input.comment_id)
            .ok_or_else(|| PlanDocumentError::CommentNotFound(input.comment_id.clone()))?;
        let prior_state = self.document.comments[index].state.clone();
        require_comment_transition(&prior_state, &input.state)?;
        let resolution_agent_run_id = if matches!(
            input.state,
            PlanCommentState::Addressed | PlanCommentState::Resolved
        ) {
            Some(input.agent_run_id.clone().ok_or_else(|| {
                PlanDocumentError::Invalid(
                    "addressed and resolved comments require agent_run_id".to_string(),
                )
            })?)
        } else {
            None
        };
        let parent = self.document.current_revision.revision_id.clone();
        self.document.comments[index].state = input.state.clone();
        self.document.comments[index].updated_at = input.created_at.clone();
        if let Some(agent_run_id) = resolution_agent_run_id.as_ref() {
            self.document
                .resolution_receipts
                .push(PlanCommentResolutionReceipt {
                    receipt_id: String::new(),
                    comment_id: input.comment_id.clone(),
                    input_revision_id: parent.clone(),
                    output_revision_id: String::new(),
                    agent_run_id: agent_run_id.clone(),
                    event_id: input.event_id.clone(),
                    explanation: input.explanation.clone(),
                    created_at: input.created_at.clone(),
                });
        }
        self.revise_unchanged_content(
            input.author,
            input.source,
            input.created_at.clone(),
            PlanRevisionOperation::CommentState {
                event_id: input.event_id.clone(),
                comment_id: input.comment_id.clone(),
                state: input.state.clone(),
            },
        )?;
        if resolution_agent_run_id.is_some() {
            let output_revision_id = self.document.current_revision.revision_id.clone();
            let receipt = self
                .document
                .resolution_receipts
                .last_mut()
                .expect("provisional resolution receipt was inserted");
            receipt.output_revision_id = output_revision_id;
            receipt.receipt_id = resolution_receipt_id(receipt)?;
        }
        self.commit(input.event_id, parent)
    }

    pub fn replay(events: &[PlanDocumentEvent]) -> Result<Self, PlanDocumentError> {
        let Some(first) = events.first() else {
            return Err(PlanDocumentError::Invalid(
                "plan document replay requires at least one event".to_string(),
            ));
        };
        let PlanDocumentEvent::Created { document, .. } = first else {
            return Err(PlanDocumentError::Replay {
                event_id: first.event_id().to_string(),
                message: "first event must be created".to_string(),
            });
        };
        if document.current_revision.parent_revision_id.is_some()
            || !matches!(
                &document.current_revision.operation,
                PlanRevisionOperation::Create { .. }
            )
            || revision_operation_event_id(&document.current_revision.operation) != first.event_id()
        {
            return Err(PlanDocumentError::Replay {
                event_id: first.event_id().to_string(),
                message: "created event must contain a root create revision".to_string(),
            });
        }
        document
            .validate()
            .map_err(|error| PlanDocumentError::Replay {
                event_id: first.event_id().to_string(),
                message: error.to_string(),
            })?;
        let mut current = document.clone();
        let mut event_ids = BTreeSet::from([first.event_id()]);
        for event in &events[1..] {
            let PlanDocumentEvent::Updated {
                event_id,
                input_revision_id,
                document,
            } = event
            else {
                return Err(PlanDocumentError::Replay {
                    event_id: event.event_id().to_string(),
                    message: "created event may only appear first".to_string(),
                });
            };
            if !event_ids.insert(event_id.as_str()) {
                return Err(PlanDocumentError::Replay {
                    event_id: event_id.clone(),
                    message: "event_id values must be unique".to_string(),
                });
            }
            if matches!(
                &document.current_revision.operation,
                PlanRevisionOperation::Create { .. }
            ) || revision_operation_event_id(&document.current_revision.operation) != event_id
            {
                return Err(PlanDocumentError::Replay {
                    event_id: event_id.clone(),
                    message: "event envelope does not match revision operation".to_string(),
                });
            }
            if input_revision_id != &current.current_revision.revision_id {
                return Err(PlanDocumentError::Replay {
                    event_id: event_id.clone(),
                    message: format!(
                        "expected input revision {}, found {}",
                        current.current_revision.revision_id, input_revision_id
                    ),
                });
            }
            if document.document_id != current.document_id
                || document.created_at != current.created_at
                || document.current_revision.parent_revision_id.as_deref()
                    != Some(input_revision_id.as_str())
            {
                return Err(PlanDocumentError::Replay {
                    event_id: event_id.clone(),
                    message: "document identity or revision lineage changed".to_string(),
                });
            }
            let prior_receipt_ids = current
                .resolution_receipts
                .iter()
                .map(|receipt| receipt.receipt_id.as_str())
                .collect::<BTreeSet<_>>();
            if current.comments.iter().any(|prior| {
                !document
                    .comments
                    .iter()
                    .any(|comment| comment.comment_id == prior.comment_id)
            }) || current.resolution_receipts.iter().any(|prior| {
                !document
                    .resolution_receipts
                    .iter()
                    .any(|receipt| receipt.receipt_id == prior.receipt_id)
            }) {
                return Err(PlanDocumentError::Replay {
                    event_id: event_id.clone(),
                    message: "comments and resolution receipts are append-only".to_string(),
                });
            }
            for prior in &current.comments {
                let comment = document
                    .comments
                    .iter()
                    .find(|comment| comment.comment_id == prior.comment_id)
                    .expect("append-only comment was checked");
                if prior.anchor != comment.anchor
                    || prior.body != comment.body
                    || prior.author != comment.author
                    || prior.created_at != comment.created_at
                {
                    return Err(PlanDocumentError::Replay {
                        event_id: event_id.clone(),
                        message: "comment identity fields changed during replay".to_string(),
                    });
                }
                if prior.state != comment.state {
                    require_comment_transition(&prior.state, &comment.state).map_err(|error| {
                        PlanDocumentError::Replay {
                            event_id: event_id.clone(),
                            message: error.to_string(),
                        }
                    })?;
                }
            }
            for receipt in document
                .resolution_receipts
                .iter()
                .filter(|receipt| !prior_receipt_ids.contains(receipt.receipt_id.as_str()))
            {
                if receipt.input_revision_id != *input_revision_id
                    || receipt.output_revision_id != document.current_revision.revision_id
                    || receipt.event_id != *event_id
                {
                    return Err(PlanDocumentError::Replay {
                        event_id: event_id.clone(),
                        message: "resolution receipt does not bind the replayed transition"
                            .to_string(),
                    });
                }
            }
            document
                .validate()
                .map_err(|error| PlanDocumentError::Replay {
                    event_id: event_id.clone(),
                    message: error.to_string(),
                })?;
            current = document.clone();
        }
        Ok(Self {
            document: current,
            events: events.to_vec(),
        })
    }

    pub fn replay_artifacts(
        artifacts: &[crate::orchestration::ArtifactRecord],
    ) -> Result<Self, PlanDocumentError> {
        let events = artifacts
            .iter()
            .map(|artifact| {
                require(
                    artifact.kind == PLAN_DOCUMENT_ARTIFACT_KIND,
                    "plan document replay artifact has the wrong kind",
                )?;
                let data = artifact.data.clone().ok_or_else(|| {
                    PlanDocumentError::Invalid(
                        "plan document replay artifact is missing data".to_string(),
                    )
                })?;
                serde_json::from_value::<PlanDocumentEvent>(data).map_err(|error| {
                    PlanDocumentError::Invalid(format!(
                        "invalid persisted plan document event: {error}"
                    ))
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        Self::replay(&events)
    }

    fn require_revision(&self, expected: &str) -> Result<(), PlanDocumentError> {
        let current = &self.document.current_revision.revision_id;
        if expected == current {
            return Ok(());
        }
        Err(PlanDocumentError::Conflict {
            document_id: self.document.document_id.clone(),
            expected_revision_id: expected.to_string(),
            current_revision_id: current.clone(),
        })
    }

    fn revise_unchanged_content(
        &mut self,
        author: PlanAuthor,
        source: PlanSource,
        created_at: String,
        operation: PlanRevisionOperation,
    ) -> Result<(), PlanDocumentError> {
        let previous = &self.document.current_revision;
        let revision = make_revision(
            Some(previous.revision_id.clone()),
            previous.markdown.clone(),
            previous.plan.clone(),
            author,
            source,
            created_at.clone(),
            operation,
            RevisionState {
                comments: &self.document.comments,
                receipts: &self.document.resolution_receipts,
            },
        )?;
        self.document.current_revision = revision;
        self.document.updated_at = created_at;
        Ok(())
    }

    fn commit(
        &mut self,
        event_id: String,
        input_revision_id: String,
    ) -> Result<&PlanDocument, PlanDocumentError> {
        self.document.validate()?;
        self.events.push(PlanDocumentEvent::Updated {
            event_id,
            input_revision_id,
            document: self.document.clone(),
        });
        Ok(&self.document)
    }
}

fn validate_revision(revision: &PlanRevision) -> Result<(), PlanDocumentError> {
    require(
        !revision.revision_id.trim().is_empty(),
        "revision_id is required",
    )?;
    require(
        !revision.markdown.trim().is_empty(),
        "editable markdown is required",
    )?;
    require(
        !revision.author.id.trim().is_empty(),
        "revision author id is required",
    )?;
    require(
        !revision.source.kind.trim().is_empty(),
        "revision source kind is required",
    )?;
    require(
        !revision.created_at.trim().is_empty(),
        "revision created_at is required",
    )?;
    validate_plan(&revision.plan)?;
    validate_revision_operation(&revision.operation)?;
    Ok(())
}

fn validate_revision_operation(operation: &PlanRevisionOperation) -> Result<(), PlanDocumentError> {
    let (event_id, comment_id) = match operation {
        PlanRevisionOperation::Create { event_id } | PlanRevisionOperation::Edit { event_id } => {
            (event_id, None)
        }
        PlanRevisionOperation::Comment {
            event_id,
            comment_id,
        }
        | PlanRevisionOperation::CommentState {
            event_id,
            comment_id,
            ..
        } => (event_id, Some(comment_id)),
    };
    require(
        !event_id.trim().is_empty(),
        "revision operation event_id is required",
    )?;
    if let Some(comment_id) = comment_id {
        require(
            !comment_id.trim().is_empty(),
            "revision operation comment_id is required",
        )?;
    }
    Ok(())
}

fn revision_operation_event_id(operation: &PlanRevisionOperation) -> &str {
    match operation {
        PlanRevisionOperation::Create { event_id }
        | PlanRevisionOperation::Edit { event_id }
        | PlanRevisionOperation::Comment { event_id, .. }
        | PlanRevisionOperation::CommentState { event_id, .. } => event_id,
    }
}

fn validate_plan(plan: &PlanArtifact) -> Result<(), PlanDocumentError> {
    require(
        plan.type_name == "plan_artifact",
        "plan _type must be plan_artifact",
    )?;
    require(
        plan.schema_version == PLAN_SCHEMA_VERSION,
        "unsupported executable plan schema_version",
    )?;
    require(!plan.id.trim().is_empty(), "plan id is required")?;
    let mut step_ids = BTreeSet::new();
    for step in &plan.steps {
        require(!step.id.trim().is_empty(), "plan step id is required")?;
        require(
            step_ids.insert(step.id.as_str()),
            "plan step id values must be unique",
        )?;
        require(
            !step.content.trim().is_empty(),
            "plan step content is required",
        )?;
        if let Some(priority) = &step.priority {
            require(
                priority.is_null()
                    || priority.is_string()
                    || priority.is_i64()
                    || priority.is_u64(),
                "plan step priority must be a string, integer, or null",
            )?;
        }
    }
    Ok(())
}

fn validate_comment(
    comment: &PlanComment,
    revision: &PlanRevision,
) -> Result<(), PlanDocumentError> {
    require(
        !comment.comment_id.trim().is_empty(),
        "comment_id is required",
    )?;
    require(!comment.body.trim().is_empty(), "comment body is required")?;
    require(
        !comment.author.id.trim().is_empty(),
        "comment author id is required",
    )?;
    let anchor = &comment.anchor;
    require(
        anchor.step_id.is_some() || anchor.quoted_text.is_some() || anchor.range.is_some(),
        "comment anchor requires step_id, quoted_text, or range",
    )?;
    let step_matches = anchor
        .step_id
        .as_deref()
        .is_some_and(|step_id| revision.plan.steps.iter().any(|step| step.id == step_id));
    let quote_is_usable = anchor
        .quoted_text
        .as_deref()
        .is_some_and(|quoted_text| !quoted_text.is_empty());
    let range_is_usable = anchor.range.as_ref().is_some_and(|range| {
        range.start < range.end
            && range.end <= revision.markdown.len()
            && revision.markdown.is_char_boundary(range.start)
            && revision.markdown.is_char_boundary(range.end)
    });
    require(
        step_matches || quote_is_usable || range_is_usable,
        "comment anchor has no usable step, quote, or range fallback",
    )?;
    Ok(())
}

fn require_comment_transition(
    from: &PlanCommentState,
    to: &PlanCommentState,
) -> Result<(), PlanDocumentError> {
    let allowed = matches!(
        (from, to),
        (
            PlanCommentState::Open | PlanCommentState::Reopened,
            PlanCommentState::Addressed | PlanCommentState::Resolved
        ) | (
            PlanCommentState::Addressed,
            PlanCommentState::Resolved | PlanCommentState::Reopened
        ) | (PlanCommentState::Resolved, PlanCommentState::Reopened)
    );
    if allowed {
        Ok(())
    } else {
        Err(PlanDocumentError::InvalidCommentTransition {
            from: from.clone(),
            to: to.clone(),
        })
    }
}

#[derive(Clone, Copy)]
struct RevisionState<'a> {
    comments: &'a [PlanComment],
    receipts: &'a [PlanCommentResolutionReceipt],
}

fn make_revision(
    parent_revision_id: Option<String>,
    markdown: String,
    plan: PlanArtifact,
    author: PlanAuthor,
    source: PlanSource,
    created_at: String,
    operation: PlanRevisionOperation,
    state: RevisionState<'_>,
) -> Result<PlanRevision, PlanDocumentError> {
    let mut revision = PlanRevision {
        revision_id: String::new(),
        parent_revision_id,
        markdown,
        plan,
        author,
        source,
        created_at,
        operation,
    };
    revision.revision_id = revision_id(&revision, state)?;
    Ok(revision)
}

fn revision_id(
    revision: &PlanRevision,
    state: RevisionState<'_>,
) -> Result<String, PlanDocumentError> {
    let receipt_state = state
        .receipts
        .iter()
        .map(|receipt| {
            serde_json::json!({
                "comment_id": receipt.comment_id,
                "input_revision_id": receipt.input_revision_id,
                "agent_run_id": receipt.agent_run_id,
                "event_id": receipt.event_id,
                "explanation": receipt.explanation,
                "created_at": receipt.created_at,
            })
        })
        .collect::<Vec<_>>();
    stable_id(
        "plan_revision",
        &serde_json::json!({
            "parent_revision_id": revision.parent_revision_id,
            "markdown": revision.markdown,
            "plan": revision.plan,
            "author": revision.author,
            "source": revision.source,
            "created_at": revision.created_at,
            "operation": revision.operation,
            "comments": state.comments,
            "resolution_receipts": receipt_state,
        }),
    )
}

fn validate_revision_identity(
    revision: &PlanRevision,
    comments: &[PlanComment],
    receipts: &[PlanCommentResolutionReceipt],
) -> Result<(), PlanDocumentError> {
    let expected = revision_id(revision, RevisionState { comments, receipts })?;
    require(
        revision.revision_id == expected,
        "revision_id does not match immutable document state",
    )
}

fn resolution_receipt_id(
    receipt: &PlanCommentResolutionReceipt,
) -> Result<String, PlanDocumentError> {
    stable_id(
        "plan_receipt",
        &serde_json::json!({
            "comment_id": receipt.comment_id,
            "input_revision_id": receipt.input_revision_id,
            "output_revision_id": receipt.output_revision_id,
            "agent_run_id": receipt.agent_run_id,
            "event_id": receipt.event_id,
        }),
    )
}

#[expect(clippy::string_slice, reason = "hex digest is ASCII")]
fn stable_id(prefix: &str, value: &serde_json::Value) -> Result<String, PlanDocumentError> {
    let canonical = crate::canonical_json::to_vec(value);
    let digest = hex::encode(Sha256::digest(canonical));
    Ok(format!("{prefix}_{}", &digest[..16]))
}

fn require(condition: bool, message: &str) -> Result<(), PlanDocumentError> {
    if condition {
        Ok(())
    } else {
        Err(PlanDocumentError::Invalid(message.to_string()))
    }
}

pub fn plan_document_json_schema() -> serde_json::Value {
    serde_json::json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "$id": "https://harnlang.com/schemas/plan-document-v1.json",
        "title": "Harn collaborative plan document",
        "type": "object",
        "additionalProperties": false,
        "required": [
            "_type", "schema_version", "document_id", "current_revision",
            "comments", "resolution_receipts", "created_at", "updated_at"
        ],
        "properties": {
            "_type": {"const": PLAN_DOCUMENT_ARTIFACT_KIND},
            "schema_version": {"const": PLAN_DOCUMENT_SCHEMA_VERSION},
            "document_id": {"type": "string", "minLength": 1},
            "current_revision": {"$ref": "#/$defs/revision"},
            "comments": {"type": "array", "items": {"$ref": "#/$defs/comment"}},
            "resolution_receipts": {
                "type": "array",
                "items": {"$ref": "#/$defs/resolution_receipt"}
            },
            "created_at": {"type": "string", "minLength": 1},
            "updated_at": {"type": "string", "minLength": 1}
        },
        "$defs": {
            "author": {
                "type": "object",
                "additionalProperties": false,
                "required": ["id"],
                "properties": {
                    "id": {"type": "string", "minLength": 1},
                    "display_name": {"type": "string"}
                }
            },
            "source": {
                "type": "object",
                "additionalProperties": false,
                "required": ["kind"],
                "properties": {
                    "kind": {"type": "string", "minLength": 1},
                    "uri": {"type": "string"}
                }
            },
            "plan_step": {
                "type": "object",
                "additionalProperties": false,
                "required": ["id", "content", "status", "priority"],
                "properties": {
                    "id": {"type": "string", "minLength": 1},
                    "content": {"type": "string", "minLength": 1},
                    "status": {
                        "enum": ["pending", "in_progress", "completed", "blocked", "cancelled"]
                    },
                    "priority": {"type": ["string", "integer", "null"]}
                }
            },
            "approval": {
                "type": "object",
                "additionalProperties": false,
                "required": ["state"],
                "properties": {
                    "state": {"enum": ["unrequested", "requested", "approved", "rejected"]},
                    "request_id": {"type": "string"},
                    "reviewer": {"type": "string"},
                    "reviewers": {"type": "array", "items": {"type": "string"}},
                    "approved_at": {"type": "string"},
                    "reason": {"type": "string"}
                }
            },
            "plan_artifact": {
                "type": "object",
                "additionalProperties": false,
                "required": [
                    "_type", "schema_version", "id", "tool", "title", "summary",
                    "steps", "assumptions", "open_questions", "verification_commands",
                    "approval"
                ],
                "properties": {
                    "_type": {"const": "plan_artifact"},
                    "schema_version": {"const": PLAN_SCHEMA_VERSION},
                    "id": {"type": "string", "minLength": 1},
                    "tool": {"type": "string"},
                    "title": {"type": "string"},
                    "summary": {"type": "string"},
                    "steps": {"type": "array", "items": {"$ref": "#/$defs/plan_step"}},
                    "assumptions": {"type": "array", "items": {"type": "string"}},
                    "open_questions": {"type": "array", "items": {"type": "string"}},
                    "verification_commands": {
                        "type": "array", "items": {"type": "string"}
                    },
                    "approval": {"$ref": "#/$defs/approval"}
                }
            },
            "revision": {
                "type": "object",
                "additionalProperties": false,
                "required": [
                    "revision_id", "markdown", "plan", "author", "source", "created_at",
                    "operation"
                ],
                "properties": {
                    "revision_id": {"type": "string", "minLength": 1},
                    "parent_revision_id": {"type": "string", "minLength": 1},
                    "markdown": {"type": "string", "minLength": 1},
                    "plan": {"$ref": "#/$defs/plan_artifact"},
                    "author": {"$ref": "#/$defs/author"},
                    "source": {"$ref": "#/$defs/source"},
                    "created_at": {"type": "string", "minLength": 1},
                    "operation": {
                        "type": "object",
                        "additionalProperties": false,
                        "required": ["kind", "event_id"],
                        "properties": {
                            "kind": {
                                "enum": ["create", "edit", "comment", "comment_state"]
                            },
                            "event_id": {"type": "string", "minLength": 1},
                            "comment_id": {"type": "string", "minLength": 1},
                            "state": {
                                "enum": ["open", "addressed", "resolved", "reopened"]
                            }
                        }
                    }
                }
            },
            "anchor": {
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "step_id": {"type": "string", "minLength": 1},
                    "quoted_text": {"type": "string", "minLength": 1},
                    "range": {
                        "type": "object",
                        "additionalProperties": false,
                        "required": ["start", "end"],
                        "properties": {
                            "start": {"type": "integer", "minimum": 0},
                            "end": {"type": "integer", "minimum": 1}
                        }
                    }
                },
                "anyOf": [
                    {"required": ["step_id"]},
                    {"required": ["quoted_text"]},
                    {"required": ["range"]}
                ]
            },
            "comment": {
                "type": "object",
                "additionalProperties": false,
                "required": [
                    "comment_id", "anchor", "body", "state", "author",
                    "created_at", "updated_at"
                ],
                "properties": {
                    "comment_id": {"type": "string", "minLength": 1},
                    "anchor": {"$ref": "#/$defs/anchor"},
                    "body": {"type": "string", "minLength": 1},
                    "state": {"enum": ["open", "addressed", "resolved", "reopened"]},
                    "author": {"$ref": "#/$defs/author"},
                    "created_at": {"type": "string", "minLength": 1},
                    "updated_at": {"type": "string", "minLength": 1}
                }
            },
            "resolution_receipt": {
                "type": "object",
                "additionalProperties": false,
                "required": [
                    "receipt_id", "comment_id", "input_revision_id",
                    "output_revision_id", "agent_run_id", "event_id", "created_at"
                ],
                "properties": {
                    "receipt_id": {"type": "string", "minLength": 1},
                    "comment_id": {"type": "string", "minLength": 1},
                    "input_revision_id": {"type": "string", "minLength": 1},
                    "output_revision_id": {"type": "string", "minLength": 1},
                    "agent_run_id": {"type": "string", "minLength": 1},
                    "event_id": {"type": "string", "minLength": 1},
                    "explanation": {"type": "string"},
                    "created_at": {"type": "string", "minLength": 1}
                }
            }
        }
    })
}

pub fn plan_document_schema_contract() -> BTreeMap<&'static str, &'static str> {
    BTreeMap::from([
        ("artifact_kind", PLAN_DOCUMENT_ARTIFACT_KIND),
        ("schema_version", PLAN_DOCUMENT_SCHEMA_VERSION),
    ])
}

#[cfg(test)]
mod tests;
