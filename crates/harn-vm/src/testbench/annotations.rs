//! Annotation sidecar for testbench tapes.
//!
//! An annotation file (`<tape>.annotations.jsonl`) is the durable form of
//! human judgment over a recorded run. Each annotation references a tape
//! event by its immutable [`TapeRecord::seq`] number and carries a
//! structured kind + evidence + author so downstream pipelines (eval
//! rubrics, friction roll-ups, crystallization candidate detection,
//! replay-for-teaching) can read the same artifact.
//!
//! ## File layout
//!
//! ```text
//! run.tape                    # the unified event tape
//! run.tape.annotations.jsonl  # annotations sidecar (this format)
//! ```
//!
//! Like the tape itself, the annotations file is line-delimited JSON. The
//! first line is a header; every subsequent line is one annotation. Empty
//! lines and lines starting with `#` are tolerated so external tools can
//! emit comments without breaking interop.
//!
//! ## Schema
//!
//! - **Header** (one line, always first):
//!
//!   ```json
//!   {
//!     "type": "header",
//!     "schema_version": 1,
//!     "tape_path": "run.tape",
//!     "tape_content_hash": "<blake3>",
//!     "harn_version": "0.8.6"
//!   }
//!   ```
//!
//! - **Annotation** (zero or more, after the header):
//!
//!   ```json
//!   {
//!     "type": "annotation",
//!     "id": "ann_001",
//!     "event_id": 42,
//!     "kind": "hypothesis",
//!     "evidence": "checkout incident — see runbook",
//!     "author": {"id": "alice", "kind": "human", "surface": "burin-code"},
//!     "timestamp": "2026-05-10T17:00:00Z",
//!     "hypothesis_status": "active"
//!   }
//!   ```
//!
//! Optional fields default to their `None` / empty form so older readers
//! roll forward when newer fields appear. Unknown [`AnnotationKind`]
//! values surface as [`AnnotationKind::Unknown`] so a validator can
//! still report on the rest.
//!
//! [`TapeRecord::seq`]: super::tape::TapeRecord::seq

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use serde::{Deserialize, Serialize};

use super::tape::{EventTape, TapeRecord};
use crate::orchestration::{
    friction_kind_allowed, FrictionEvent, FrictionLink, FRICTION_SCHEMA_VERSION,
};

/// Format version of the annotations sidecar. Bump on any breaking
/// change. Loaders refuse files with a higher version.
pub const ANNOTATION_SCHEMA_VERSION: u32 = 1;

/// Conventional sidecar suffix appended to a tape path. `run.tape`
/// pairs with `run.tape.annotations.jsonl`.
pub const ANNOTATIONS_SIDECAR_SUFFIX: &str = ".annotations.jsonl";

/// Header record at the top of every annotations file. Captures the
/// schema version and a back-reference to the tape so a validator can
/// detect mismatched bundles.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AnnotationHeader {
    pub schema_version: u32,
    /// Tape this annotation set targets. Stored as the path the
    /// annotation author saw — the validator resolves it relative to the
    /// annotations file.
    #[serde(default)]
    pub tape_path: Option<String>,
    /// BLAKE3 hex digest of the tape's NDJSON body when the annotations
    /// were written. The validator uses this to spot tape edits that
    /// invalidate event_id references.
    #[serde(default)]
    pub tape_content_hash: Option<String>,
    /// `harn-vm` `CARGO_PKG_VERSION` of the producer. Informational.
    #[serde(default)]
    pub harn_version: Option<String>,
}

impl AnnotationHeader {
    pub fn current(tape_path: Option<String>, tape_content_hash: Option<String>) -> Self {
        Self {
            schema_version: ANNOTATION_SCHEMA_VERSION,
            tape_path,
            tape_content_hash,
            harn_version: Some(env!("CARGO_PKG_VERSION").to_string()),
        }
    }
}

/// One annotation record on a tape event. Every field except `event_id`
/// and `kind` is optional so authoring tools can emit minimal records.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Annotation {
    /// Stable id for the annotation. Defaults to `ann_{event_id}_{seq}`
    /// when authors don't pick one — the only requirement is uniqueness
    /// within a file.
    #[serde(default)]
    pub id: String,
    /// Tape event the annotation targets. Matches [`TapeRecord::seq`].
    pub event_id: u64,
    pub kind: AnnotationKind,
    /// Free-text evidence (markdown allowed). Authors may also pass
    /// structured links via [`Annotation::links`].
    #[serde(default)]
    pub evidence: Option<String>,
    /// Optional structured fix suggestion. Free-form JSON so a candidate
    /// edit, a missing context-pack entry, or a tool-call patch can ride
    /// on the same record without inventing per-kind shapes.
    #[serde(default)]
    pub suggested_fix: Option<serde_json::Value>,
    #[serde(default)]
    pub author: Option<AnnotationAuthor>,
    /// RFC-3339 timestamp. Defaults to the moment the annotation was
    /// authored; downstream pipelines treat missing values as "unknown".
    #[serde(default)]
    pub timestamp: Option<String>,
    /// Span-style annotations cover a range of events. `start_event_id`
    /// must equal `event_id`; the wrapping is intentional so single-event
    /// annotations and span annotations share a row shape.
    #[serde(default)]
    pub span: Option<AnnotationSpan>,
    /// Required when `kind == hypothesis`; ignored otherwise.
    #[serde(default)]
    pub hypothesis_status: Option<HypothesisStatus>,
    /// Required when `kind == friction`; matches the friction-event
    /// taxonomy in [`crate::orchestration::friction`] so a bag of
    /// annotations + a bag of FrictionEvents are interchangeable.
    #[serde(default)]
    pub friction_kind: Option<String>,
    /// Optional structured links to runbooks, dashboards, tickets, or
    /// upstream incidents. Authors who only have prose put it in
    /// `evidence`.
    #[serde(default)]
    pub links: Vec<AnnotationLink>,
    /// Free-form metadata for downstream consumers. Kept open-ended on
    /// purpose — the eval rubric, persona quality dashboard, and
    /// crystallization detector each tag annotations differently.
    #[serde(default)]
    pub metadata: BTreeMap<String, serde_json::Value>,
}

/// Closed taxonomy of annotation kinds. New kinds must be added here so
/// the validator can reason about them; older readers receive
/// [`AnnotationKind::Unknown`] for kinds they don't recognize.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "snake_case")]
pub enum AnnotationKind {
    /// "This event was correct." Eval rubric ground truth.
    Correct,
    /// "This event was wrong." Eval rubric ground truth.
    Incorrect,
    /// "Here is a better way to handle this turn." Pairs with a
    /// `suggested_fix` payload.
    Alternative,
    /// Free-text commentary with no judgment baked in.
    Note,
    /// Anchor for replay-for-teaching playback. Presenter mode pauses on
    /// markers and surfaces the evidence.
    Marker,
    /// Suppress a downstream consumer from acting on this event (e.g.
    /// silence a known-flake in a dashboard).
    Mute,
    /// Human prior to verify. Carries a `hypothesis_status`.
    Hypothesis,
    /// Operational learning signal. Carries a `friction_kind` matching
    /// the friction-event taxonomy.
    Friction,
    /// "This sequence is a candidate workflow to crystallize." Surfaced
    /// directly to the candidate-detection pipeline.
    CrystallizeHere,
    /// Catch-all for kinds emitted by a newer producer.
    #[serde(other)]
    Unknown,
}

impl AnnotationKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Correct => "correct",
            Self::Incorrect => "incorrect",
            Self::Alternative => "alternative",
            Self::Note => "note",
            Self::Marker => "marker",
            Self::Mute => "mute",
            Self::Hypothesis => "hypothesis",
            Self::Friction => "friction",
            Self::CrystallizeHere => "crystallize_here",
            Self::Unknown => "unknown",
        }
    }

    /// Parse a CLI-style kind name. Accepts the snake_case spellings the
    /// schema serializes to.
    pub fn parse_cli(input: &str) -> Result<Self, String> {
        match input {
            "correct" => Ok(Self::Correct),
            "incorrect" => Ok(Self::Incorrect),
            "alternative" => Ok(Self::Alternative),
            "note" => Ok(Self::Note),
            "marker" => Ok(Self::Marker),
            "mute" => Ok(Self::Mute),
            "hypothesis" => Ok(Self::Hypothesis),
            "friction" => Ok(Self::Friction),
            "crystallize_here" => Ok(Self::CrystallizeHere),
            other => Err(format!(
                "unknown annotation kind '{other}' (expected one of correct, incorrect, alternative, note, marker, mute, hypothesis, friction, crystallize_here)"
            )),
        }
    }
}

/// Lifecycle of a hypothesis-kind annotation. Mirrors the
/// human-hypothesis loop in harn-cloud#54 / burin-code#277.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum HypothesisStatus {
    /// Author posed the hypothesis; no verification yet.
    Active,
    /// An agent is gathering evidence.
    Verifying,
    /// Evidence supports the hypothesis.
    Confirmed,
    /// Evidence rules the hypothesis out.
    Disproven,
    /// Hypothesis aged out without a resolution.
    Stale,
}

/// Span over a contiguous range of events. `start_event_id` must equal
/// the wrapping annotation's `event_id`; `end_event_id` must be greater
/// than or equal to the start. The validator enforces both invariants.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AnnotationSpan {
    pub start_event_id: u64,
    pub end_event_id: u64,
}

/// Provenance for an annotation. Surfaces the difference between a human
/// who clicked through Burin Code and an agent that auto-flagged a turn
/// during a self-eval.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AnnotationAuthor {
    /// Stable identifier — email, agent id, or service slug. The schema
    /// does not police format; downstream consumers do.
    #[serde(default)]
    pub id: Option<String>,
    /// Where the annotation came from.
    pub kind: AuthorKind,
    /// Surface that authored the record — `burin-code`, `harn-cloud`,
    /// `cli`, etc. Free-form so new surfaces don't need a schema bump.
    #[serde(default)]
    pub surface: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AuthorKind {
    Human,
    Agent,
    System,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct AnnotationLink {
    pub label: Option<String>,
    pub url: Option<String>,
    /// Optional ticket/issue reference (e.g. `harn#1474`,
    /// `INGEST-321`). Kept separate from `url` so the cloud surface can
    /// resolve them uniformly.
    pub reference: Option<String>,
}

/// Fully-loaded annotation tape — header plus every record. Built by
/// [`AnnotationTape::load`] and consumed by the validator, the replay
/// surface, and the export pipeline.
#[derive(Debug, Clone)]
pub struct AnnotationTape {
    pub header: AnnotationHeader,
    pub annotations: Vec<Annotation>,
}

impl AnnotationTape {
    pub fn new(header: AnnotationHeader) -> Self {
        Self {
            header,
            annotations: Vec::new(),
        }
    }

    /// Persist the tape as `path.annotations.jsonl`-style NDJSON.
    pub fn persist(&self, path: &Path) -> Result<(), String> {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)
                    .map_err(|err| format!("mkdir {}: {err}", parent.display()))?;
            }
        }
        let mut body = String::new();
        body.push_str(
            &serde_json::to_string(&AnnotationLine::Header(self.header.clone()))
                .map_err(|err| format!("serialize annotation header: {err}"))?,
        );
        body.push('\n');
        for annotation in &self.annotations {
            body.push_str(
                &serde_json::to_string(&AnnotationLine::Annotation(annotation.clone()))
                    .map_err(|err| format!("serialize annotation: {err}"))?,
            );
            body.push('\n');
        }
        std::fs::write(path, body).map_err(|err| format!("write {}: {err}", path.display()))
    }

    /// Read an annotation file. Empty lines and `#`-prefixed comment
    /// lines are skipped so authors can group records visually.
    pub fn load(path: &Path) -> Result<Self, String> {
        let body = std::fs::read_to_string(path)
            .map_err(|err| format!("read {}: {err}", path.display()))?;
        let mut lines = body.lines().enumerate().filter(|(_, line)| {
            let trimmed = line.trim();
            !trimmed.is_empty() && !trimmed.starts_with('#')
        });
        let (header_idx, header_line) = lines.next().ok_or_else(|| {
            format!(
                "empty annotation file: {} (expected a header on line 1)",
                path.display()
            )
        })?;
        let parsed_header: AnnotationLine =
            serde_json::from_str(header_line.trim()).map_err(|err| {
                format!(
                    "parse annotation header at line {} in {}: {err}",
                    header_idx + 1,
                    path.display()
                )
            })?;
        let header = match parsed_header {
            AnnotationLine::Header(header) => header,
            AnnotationLine::Annotation(_) => {
                return Err(format!(
                    "annotation file {} is missing its header (first non-empty line is a record)",
                    path.display()
                ))
            }
        };
        if header.schema_version > ANNOTATION_SCHEMA_VERSION {
            return Err(format!(
                "annotation file {} declares schema_version {} but this runtime supports up to {ANNOTATION_SCHEMA_VERSION}",
                path.display(),
                header.schema_version
            ));
        }

        let mut annotations = Vec::new();
        for (idx, line) in lines {
            let parsed: AnnotationLine = serde_json::from_str(line.trim()).map_err(|err| {
                format!(
                    "parse annotation at line {} in {}: {err}",
                    idx + 1,
                    path.display()
                )
            })?;
            match parsed {
                AnnotationLine::Annotation(annotation) => annotations.push(annotation),
                AnnotationLine::Header(_) => {
                    return Err(format!(
                        "annotation file {} contains a second header at line {}",
                        path.display(),
                        idx + 1
                    ))
                }
            }
        }
        Ok(Self {
            header,
            annotations,
        })
    }

    /// Filter annotations by kind. Used by the export pipeline.
    pub fn filter_by_kind<'a>(
        &'a self,
        kind: AnnotationKind,
    ) -> impl Iterator<Item = &'a Annotation> + 'a {
        self.annotations
            .iter()
            .filter(move |annotation| annotation.kind == kind)
    }

    /// Convert friction-kind annotations into [`FrictionEvent`]s so they
    /// flow into [`crate::orchestration::generate_context_pack_suggestions`]
    /// and the friction roll-up dashboard alongside natively-emitted
    /// events.
    pub fn to_friction_events(&self) -> Vec<FrictionEvent> {
        self.filter_by_kind(AnnotationKind::Friction)
            .filter_map(|annotation| annotation_to_friction_event(annotation, &self.header))
            .collect()
    }

    /// Anchor seqs flagged by `crystallize_here` annotations. The
    /// crystallization candidate detector keys on these to bias toward
    /// human-curated workflow-shaped sequences over inferred ones.
    pub fn crystallize_anchors(&self) -> Vec<CrystallizeAnchor> {
        self.filter_by_kind(AnnotationKind::CrystallizeHere)
            .map(|annotation| CrystallizeAnchor {
                event_id: annotation.event_id,
                end_event_id: annotation
                    .span
                    .as_ref()
                    .map(|span| span.end_event_id)
                    .unwrap_or(annotation.event_id),
                evidence: annotation.evidence.clone(),
                author: annotation.author.clone(),
                metadata: annotation.metadata.clone(),
            })
            .collect()
    }
}

/// One event the human-judgment loop has flagged as worth crystallizing.
/// The candidate detector consumes a `Vec<CrystallizeAnchor>` alongside
/// inferred candidates so the two paths converge into one ranked list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CrystallizeAnchor {
    pub event_id: u64,
    pub end_event_id: u64,
    pub evidence: Option<String>,
    pub author: Option<AnnotationAuthor>,
    pub metadata: BTreeMap<String, serde_json::Value>,
}

/// One on-disk line in the annotations file. Tagged-enum dispatch keeps
/// the file homogeneous JSONL.
///
/// The size disparity between `Header` and `Annotation` is intentional:
/// every JSONL file starts with exactly one `Header`, then any number of
/// `Annotation` lines. Boxing `Annotation` would add a heap indirection
/// to the hot deserialize loop to save a few bytes on the one-off header
/// — an obvious lose. Surfaced by the host-target compile of `harn-vm`
/// introduced when `harn-cli`'s build script gained `harn-vm` as a
/// build-dep for the AOT bytecode embedding pass (G7 / harn#2300).
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum AnnotationLine {
    Header(AnnotationHeader),
    Annotation(Annotation),
}

/// Validation problem detected by [`validate_against_tape`]. Each variant
/// carries enough context for a CLI report or a CI annotation comment.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "code", rename_all = "snake_case")]
pub enum AnnotationProblem {
    /// Schema-level error (missing required field, malformed enum). The
    /// loader rejects most of these, but a few only surface once we
    /// cross-reference with the tape (e.g. `event_id` out of range).
    Schema {
        annotation_id: String,
        message: String,
    },
    /// `event_id` does not match any record in the tape.
    UnknownEventId {
        annotation_id: String,
        event_id: u64,
    },
    /// `kind == hypothesis` but `hypothesis_status` is missing.
    HypothesisStatusMissing { annotation_id: String },
    /// `kind != hypothesis` but `hypothesis_status` is set.
    HypothesisStatusUnexpected { annotation_id: String },
    /// `kind == friction` but `friction_kind` is missing.
    FrictionKindMissing { annotation_id: String },
    /// `kind != friction` but `friction_kind` is set.
    FrictionKindUnexpected { annotation_id: String },
    /// `friction_kind` is set but does not match the
    /// [`friction::FRICTION_KINDS`] taxonomy.
    FrictionKindUnknown {
        annotation_id: String,
        friction_kind: String,
    },
    /// `span` shape is malformed (start != event_id, end < start, or end
    /// out of range).
    InvalidSpan {
        annotation_id: String,
        message: String,
    },
    /// Two annotations share the same `id`.
    DuplicateId { annotation_id: String },
    /// Header references a tape whose content hash does not match the
    /// loaded tape. Indicates the tape was edited after annotations were
    /// authored — references may be stale.
    TapeDigestMismatch { expected: String, actual: String },
    /// `AnnotationKind::Unknown` records were preserved on load but the
    /// validator can't reason about them.
    UnknownKind { annotation_id: String },
}

/// Result of validating annotations against a tape.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AnnotationValidationReport {
    pub annotations_checked: usize,
    pub problems: Vec<AnnotationProblem>,
    /// Counts per-kind so reporting can show a quick taxonomy summary.
    pub kind_counts: BTreeMap<String, usize>,
}

impl AnnotationValidationReport {
    pub fn is_ok(&self) -> bool {
        self.problems.is_empty()
    }
}

/// Validate an annotations file against its target tape. Returns a
/// structured report so the CLI can emit either pretty-printed problems
/// or a machine-readable JSON payload.
pub fn validate_against_tape(
    annotations: &AnnotationTape,
    tape: &EventTape,
) -> AnnotationValidationReport {
    let event_seqs: BTreeSet<u64> = tape.records.iter().map(|record| record.seq).collect();
    let max_seq = event_seqs.iter().max().copied();
    let mut problems = Vec::new();
    let mut seen_ids: BTreeSet<String> = BTreeSet::new();
    let mut kind_counts: BTreeMap<String, usize> = BTreeMap::new();

    for annotation in &annotations.annotations {
        let id_for_report = if annotation.id.is_empty() {
            format!("ann@event_{}", annotation.event_id)
        } else {
            annotation.id.clone()
        };
        *kind_counts
            .entry(annotation.kind.as_str().to_string())
            .or_insert(0) += 1;

        if !annotation.id.is_empty() && !seen_ids.insert(annotation.id.clone()) {
            problems.push(AnnotationProblem::DuplicateId {
                annotation_id: id_for_report.clone(),
            });
        }

        if !event_seqs.contains(&annotation.event_id) {
            problems.push(AnnotationProblem::UnknownEventId {
                annotation_id: id_for_report.clone(),
                event_id: annotation.event_id,
            });
        }

        match annotation.kind {
            AnnotationKind::Hypothesis => {
                if annotation.hypothesis_status.is_none() {
                    problems.push(AnnotationProblem::HypothesisStatusMissing {
                        annotation_id: id_for_report.clone(),
                    });
                }
                if annotation.friction_kind.is_some() {
                    problems.push(AnnotationProblem::FrictionKindUnexpected {
                        annotation_id: id_for_report.clone(),
                    });
                }
            }
            AnnotationKind::Friction => {
                if annotation.hypothesis_status.is_some() {
                    problems.push(AnnotationProblem::HypothesisStatusUnexpected {
                        annotation_id: id_for_report.clone(),
                    });
                }
                match annotation.friction_kind.as_deref() {
                    None => problems.push(AnnotationProblem::FrictionKindMissing {
                        annotation_id: id_for_report.clone(),
                    }),
                    Some(kind) if !friction_kind_allowed(kind) => {
                        problems.push(AnnotationProblem::FrictionKindUnknown {
                            annotation_id: id_for_report.clone(),
                            friction_kind: kind.to_string(),
                        })
                    }
                    Some(_) => {}
                }
            }
            AnnotationKind::Unknown => {
                problems.push(AnnotationProblem::UnknownKind {
                    annotation_id: id_for_report.clone(),
                });
            }
            _ => {
                if annotation.hypothesis_status.is_some() {
                    problems.push(AnnotationProblem::HypothesisStatusUnexpected {
                        annotation_id: id_for_report.clone(),
                    });
                }
                if annotation.friction_kind.is_some() {
                    problems.push(AnnotationProblem::FrictionKindUnexpected {
                        annotation_id: id_for_report.clone(),
                    });
                }
            }
        }

        if let Some(span) = annotation.span.as_ref() {
            if span.start_event_id != annotation.event_id {
                problems.push(AnnotationProblem::InvalidSpan {
                    annotation_id: id_for_report.clone(),
                    message: format!(
                        "span.start_event_id ({}) must equal event_id ({})",
                        span.start_event_id, annotation.event_id
                    ),
                });
            }
            if span.end_event_id < span.start_event_id {
                problems.push(AnnotationProblem::InvalidSpan {
                    annotation_id: id_for_report.clone(),
                    message: format!(
                        "span.end_event_id ({}) is before start_event_id ({})",
                        span.end_event_id, span.start_event_id
                    ),
                });
            }
            if let Some(max) = max_seq {
                if span.end_event_id > max {
                    problems.push(AnnotationProblem::InvalidSpan {
                        annotation_id: id_for_report.clone(),
                        message: format!(
                            "span.end_event_id ({}) is past the last tape event (seq={max})",
                            span.end_event_id
                        ),
                    });
                }
            }
        }
    }

    if let (Some(expected), Some(actual)) = (
        annotations.header.tape_content_hash.as_deref(),
        compute_tape_content_hash(tape).as_deref(),
    ) {
        if expected != actual {
            problems.push(AnnotationProblem::TapeDigestMismatch {
                expected: expected.to_string(),
                actual: actual.to_string(),
            });
        }
    }

    AnnotationValidationReport {
        annotations_checked: annotations.annotations.len(),
        problems,
        kind_counts,
    }
}

/// BLAKE3 hex digest of a tape's logical content. Implemented by
/// hashing the deterministically-serialized record stream so the digest
/// is stable across runs that produce the same tape — and changes when
/// any record content does.
pub fn compute_tape_content_hash(tape: &EventTape) -> Option<String> {
    let mut hasher = blake3::Hasher::new();
    for record in &tape.records {
        let line = serde_json::to_vec(record).ok()?;
        hasher.update(&line);
        hasher.update(b"\n");
    }
    Some(hasher.finalize().to_hex().to_string())
}

/// Convenience: pair a tape record with all annotations that reference
/// its seq. Used by the replay surface and the export pipeline.
pub fn annotations_for_record<'a>(
    annotations: &'a AnnotationTape,
    record: &TapeRecord,
) -> Vec<&'a Annotation> {
    annotations
        .annotations
        .iter()
        .filter(|annotation| annotation.event_id == record.seq)
        .collect()
}

/// Adapt a friction-kind annotation into a [`FrictionEvent`]. Returns
/// `None` when the annotation is not a friction record or is missing the
/// required `friction_kind`.
pub fn annotation_to_friction_event(
    annotation: &Annotation,
    header: &AnnotationHeader,
) -> Option<FrictionEvent> {
    if annotation.kind != AnnotationKind::Friction {
        return None;
    }
    let kind = annotation.friction_kind.clone()?;
    if !friction_kind_allowed(&kind) {
        return None;
    }
    let summary = annotation.evidence.clone().unwrap_or_else(|| {
        format!(
            "annotation {} on event {}",
            annotation.id, annotation.event_id
        )
    });
    let mut links = Vec::new();
    for link in &annotation.links {
        links.push(FrictionLink {
            label: link.label.clone(),
            url: link.url.clone(),
            trace_id: link.reference.clone(),
        });
    }
    Some(FrictionEvent {
        schema_version: FRICTION_SCHEMA_VERSION,
        id: if annotation.id.is_empty() {
            format!("annotation_{}", annotation.event_id)
        } else {
            annotation.id.clone()
        },
        kind,
        source: header.tape_path.clone(),
        actor: annotation.author.as_ref().and_then(|a| a.id.clone()),
        tenant_id: None,
        task_id: None,
        run_id: None,
        workflow_id: None,
        tool: None,
        provider: None,
        redacted_summary: summary,
        estimated_cost_usd: None,
        estimated_time_ms: None,
        recurrence_hints: Vec::new(),
        trace_id: None,
        span_id: None,
        links,
        human_hypothesis: None,
        metadata: annotation.metadata.clone(),
        timestamp: annotation
            .timestamp
            .clone()
            .unwrap_or_else(crate::orchestration::now_rfc3339),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testbench::tape::{TapeHeader, TapePhase, TapeRecord, TapeRecordKind};
    use tempfile::TempDir;

    fn sample_tape() -> EventTape {
        let mut tape = EventTape::new(TapeHeader::current(
            Some(1_700_000_000_000),
            Some("script.harn".into()),
            Vec::new(),
        ));
        for seq in 0..3 {
            tape.records.push(TapeRecord {
                seq,
                phase: TapePhase::UserScript,
                virtual_time_ms: 0,
                monotonic_ms: 0,
                kind: TapeRecordKind::ClockSleep { duration_ms: 1 },
            });
        }
        tape
    }

    fn note_annotation(id: &str, event_id: u64) -> Annotation {
        Annotation {
            id: id.into(),
            event_id,
            kind: AnnotationKind::Note,
            evidence: Some("looked fine".into()),
            suggested_fix: None,
            author: Some(AnnotationAuthor {
                id: Some("alice".into()),
                kind: AuthorKind::Human,
                surface: Some("burin-code".into()),
            }),
            timestamp: Some("2026-05-10T17:00:00Z".into()),
            span: None,
            hypothesis_status: None,
            friction_kind: None,
            links: Vec::new(),
            metadata: BTreeMap::new(),
        }
    }

    #[test]
    fn round_trip_preserves_records() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().join("run.tape.annotations.jsonl");
        let mut tape = AnnotationTape::new(AnnotationHeader::current(
            Some("run.tape".into()),
            Some("deadbeef".into()),
        ));
        tape.annotations.push(note_annotation("ann-1", 0));
        tape.annotations.push(Annotation {
            kind: AnnotationKind::Hypothesis,
            hypothesis_status: Some(HypothesisStatus::Active),
            ..note_annotation("ann-2", 1)
        });
        tape.persist(&path).unwrap();

        let loaded = AnnotationTape::load(&path).unwrap();
        assert_eq!(loaded.header.schema_version, ANNOTATION_SCHEMA_VERSION);
        assert_eq!(loaded.annotations.len(), 2);
        assert_eq!(loaded.annotations[0].kind, AnnotationKind::Note);
        assert_eq!(loaded.annotations[1].kind, AnnotationKind::Hypothesis);
        assert_eq!(
            loaded.annotations[1].hypothesis_status,
            Some(HypothesisStatus::Active)
        );
    }

    #[test]
    fn validator_flags_unknown_event_id_and_missing_status() {
        let tape = sample_tape();
        let mut annotations =
            AnnotationTape::new(AnnotationHeader::current(Some("run.tape".into()), None));
        annotations.annotations.push(note_annotation("note", 0));
        annotations.annotations.push(Annotation {
            event_id: 99,
            kind: AnnotationKind::Hypothesis,
            hypothesis_status: None,
            ..note_annotation("missing", 99)
        });
        annotations.annotations.push(Annotation {
            kind: AnnotationKind::Friction,
            friction_kind: Some("does_not_exist".into()),
            ..note_annotation("bad-friction", 1)
        });
        annotations.annotations.push(Annotation {
            kind: AnnotationKind::Friction,
            friction_kind: None,
            ..note_annotation("missing-friction", 2)
        });

        let report = validate_against_tape(&annotations, &tape);
        assert_eq!(report.annotations_checked, 4);
        assert!(report
            .problems
            .iter()
            .any(|p| matches!(p, AnnotationProblem::UnknownEventId { event_id: 99, .. })));
        assert!(report
            .problems
            .iter()
            .any(|p| matches!(p, AnnotationProblem::HypothesisStatusMissing { .. })));
        assert!(report
            .problems
            .iter()
            .any(|p| matches!(p, AnnotationProblem::FrictionKindUnknown { .. })));
        assert!(report
            .problems
            .iter()
            .any(|p| matches!(p, AnnotationProblem::FrictionKindMissing { .. })));
    }

    #[test]
    fn span_validation_enforces_invariants() {
        let tape = sample_tape();
        let mut annotations = AnnotationTape::new(AnnotationHeader::current(None, None));
        annotations.annotations.push(Annotation {
            span: Some(AnnotationSpan {
                start_event_id: 5,
                end_event_id: 10,
            }),
            ..note_annotation("bad-start", 1)
        });
        annotations.annotations.push(Annotation {
            span: Some(AnnotationSpan {
                start_event_id: 1,
                end_event_id: 0,
            }),
            ..note_annotation("inverted", 1)
        });
        annotations.annotations.push(Annotation {
            span: Some(AnnotationSpan {
                start_event_id: 1,
                end_event_id: 99,
            }),
            ..note_annotation("past-end", 1)
        });

        let report = validate_against_tape(&annotations, &tape);
        // bad-start: start != event_id + end > max ⇒ 2 problems.
        // inverted: end < start ⇒ 1 problem.
        // past-end: end > max ⇒ 1 problem.
        assert_eq!(
            report
                .problems
                .iter()
                .filter(|p| matches!(p, AnnotationProblem::InvalidSpan { .. }))
                .count(),
            4
        );
    }

    #[test]
    fn duplicate_ids_are_flagged() {
        let tape = sample_tape();
        let mut annotations = AnnotationTape::new(AnnotationHeader::current(None, None));
        annotations.annotations.push(note_annotation("dupe", 0));
        annotations.annotations.push(note_annotation("dupe", 1));
        let report = validate_against_tape(&annotations, &tape);
        assert!(report
            .problems
            .iter()
            .any(|p| matches!(p, AnnotationProblem::DuplicateId { .. })));
    }

    #[test]
    fn tape_digest_mismatch_flags_stale_annotations() {
        let tape = sample_tape();
        let mut annotations = AnnotationTape::new(AnnotationHeader::current(
            Some("run.tape".into()),
            Some("not-the-real-hash".into()),
        ));
        annotations.annotations.push(note_annotation("note", 0));
        let report = validate_against_tape(&annotations, &tape);
        assert!(report
            .problems
            .iter()
            .any(|p| matches!(p, AnnotationProblem::TapeDigestMismatch { .. })));
    }

    #[test]
    fn unknown_kind_round_trips_and_validator_flags() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().join("future.annotations.jsonl");
        let body = format!(
            "{}\n{}\n",
            serde_json::to_string(&AnnotationLine::Header(AnnotationHeader::current(
                None, None
            )))
            .unwrap(),
            r#"{"type":"annotation","id":"ann","event_id":0,"kind":"future_kind"}"#
        );
        std::fs::write(&path, body).unwrap();
        let loaded = AnnotationTape::load(&path).unwrap();
        assert_eq!(loaded.annotations.len(), 1);
        assert_eq!(loaded.annotations[0].kind, AnnotationKind::Unknown);
        let report = validate_against_tape(&loaded, &sample_tape());
        assert!(report
            .problems
            .iter()
            .any(|p| matches!(p, AnnotationProblem::UnknownKind { .. })));
    }

    #[test]
    fn rejects_newer_schema_version() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().join("future.annotations.jsonl");
        std::fs::write(
            &path,
            r#"{"type":"header","schema_version":99}
"#,
        )
        .unwrap();
        let err = AnnotationTape::load(&path).unwrap_err();
        assert!(err.contains("schema_version 99"), "{err}");
    }

    #[test]
    fn comments_and_blank_lines_are_skipped() {
        let temp = TempDir::new().unwrap();
        let path = temp.path().join("commented.annotations.jsonl");
        let header = serde_json::to_string(&AnnotationLine::Header(AnnotationHeader::current(
            None, None,
        )))
        .unwrap();
        let annotation =
            serde_json::to_string(&AnnotationLine::Annotation(note_annotation("ann", 0))).unwrap();
        let body = format!("# leading comment\n\n{header}\n\n# spacer\n{annotation}\n");
        std::fs::write(&path, body).unwrap();
        let loaded = AnnotationTape::load(&path).unwrap();
        assert_eq!(loaded.annotations.len(), 1);
    }

    #[test]
    fn friction_annotations_round_trip_through_friction_event() {
        let mut tape =
            AnnotationTape::new(AnnotationHeader::current(Some("run.tape".into()), None));
        tape.annotations.push(Annotation {
            kind: AnnotationKind::Friction,
            friction_kind: Some("repeated_query".into()),
            evidence: Some("Splunk lookup repeats every incident".into()),
            ..note_annotation("friction-1", 2)
        });
        let events = tape.to_friction_events();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].kind, "repeated_query");
        assert_eq!(events[0].schema_version, FRICTION_SCHEMA_VERSION);
        assert_eq!(
            events[0].redacted_summary,
            "Splunk lookup repeats every incident"
        );
    }

    #[test]
    fn crystallize_anchors_surface_event_ids() {
        let mut tape = AnnotationTape::new(AnnotationHeader::current(None, None));
        tape.annotations.push(Annotation {
            kind: AnnotationKind::CrystallizeHere,
            span: Some(AnnotationSpan {
                start_event_id: 1,
                end_event_id: 4,
            }),
            ..note_annotation("crys-1", 1)
        });
        let anchors = tape.crystallize_anchors();
        assert_eq!(anchors.len(), 1);
        assert_eq!(anchors[0].event_id, 1);
        assert_eq!(anchors[0].end_event_id, 4);
    }
}
