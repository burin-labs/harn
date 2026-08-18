//! Canonical session/transcript search contract.
//!
//! Ranking, scope, fallback reporting, and searchable-text projection live
//! here so storage adapters and transports cannot grow competing policy.

use std::collections::BTreeMap;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::redaction::SharedEventRedactor;
use crate::{EventId, SessionEventKind, SessionMeta, StoredEvent};

pub const DEFAULT_SEARCH_LIMIT: usize = 50;
pub const MAX_SEARCH_LIMIT: usize = 500;
const RRF_K: f32 = 60.0;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SearchMode {
    Fts,
    Semantic,
    #[default]
    Hybrid,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchFilter {
    #[serde(default)]
    pub tenant_id: Option<String>,
    #[serde(default)]
    pub project_scope: Option<String>,
    #[serde(default)]
    pub session_id: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchQuery {
    pub query: String,
    #[serde(default)]
    pub mode: SearchMode,
    #[serde(default)]
    pub filter: SearchFilter,
    #[serde(default)]
    pub limit: Option<usize>,
}

impl SearchQuery {
    pub fn validate(&self) -> Result<(), String> {
        if self.query.trim().is_empty() {
            return Err("search query must be non-empty".to_string());
        }
        if self.query.chars().any(|character| character == '\0') {
            return Err("search query must not contain NUL".to_string());
        }
        let has_scope = [
            self.filter.tenant_id.as_deref(),
            self.filter.project_scope.as_deref(),
            self.filter.session_id.as_deref(),
        ]
        .into_iter()
        .flatten()
        .any(|scope| !scope.trim().is_empty());
        if !has_scope {
            return Err(
                "search requires tenant_id, project_scope, or session_id scope".to_string(),
            );
        }
        Ok(())
    }

    pub fn limit(&self) -> usize {
        self.limit
            .unwrap_or(DEFAULT_SEARCH_LIMIT)
            .clamp(1, MAX_SEARCH_LIMIT)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SearchHit {
    pub session_id: String,
    pub event_id: EventId,
    pub kind: SessionEventKind,
    pub score: f32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fts_score: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub semantic_score: Option<f32>,
    pub snippet: String,
    pub event: StoredEvent,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SearchResponse {
    pub requested_mode: SearchMode,
    pub effective_mode: SearchMode,
    pub embedding_backend: String,
    pub semantic_floor: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fallback_reason: Option<String>,
    pub hits: Vec<SearchHit>,
}

/// Backend-neutral embedding seam used by the store's search implementation.
///
/// The deterministic lexical backend is always available. Higher-quality
/// implementations may be injected through `StoreHooks` without changing the
/// search interface or any transport.
pub trait Embedder: Send + Sync {
    fn embed(&self, text: &str) -> Vec<f32>;
    fn dim(&self) -> usize;
    fn name(&self) -> &str;

    /// Whether this backend ranks by meaning rather than by surface form.
    ///
    /// Defaults to `false`: the claim is earned, never inherited. A backend
    /// that returns `true` is asserting it can rank a semantically related
    /// document above a lexically similar decoy, and
    /// [`conformance::audit_semantic_claim`] is the bar that assertion is
    /// measured against.
    ///
    /// The default direction matters. Under-claiming costs a caller some
    /// recall it could have had; over-claiming makes
    /// [`SearchResponse::semantic_floor`] lie, silently converts a
    /// [`SearchMode::Semantic`] query into surface matching, and suppresses
    /// the fallback notice that would otherwise tell the caller what
    /// happened. So an unconsidered backend degrades loudly instead of
    /// claiming quietly.
    fn is_semantic(&self) -> bool {
        false
    }

    fn embed_batch(&self, texts: &[String]) -> Vec<Vec<f32>> {
        texts.iter().map(|text| self.embed(text)).collect()
    }
}

/// Deterministic cross-platform lexical-hash floor.
pub struct LexicalEmbedder {
    dim: usize,
}

impl LexicalEmbedder {
    pub fn new(dim: usize) -> Self {
        Self { dim: dim.max(16) }
    }

    fn add_feature(&self, vector: &mut [f32], feature: &str, weight: f32) {
        let hash = fnv1a(feature.as_bytes(), 0);
        let bucket = (hash % self.dim as u64) as usize;
        let sign = if fnv1a(feature.as_bytes(), 0x9e37_79b9_7f4a_7c15) & 1 == 0 {
            1.0
        } else {
            -1.0
        };
        vector[bucket] += sign * weight;
    }
}

impl Default for LexicalEmbedder {
    fn default() -> Self {
        Self::new(256)
    }
}

impl Embedder for LexicalEmbedder {
    fn embed(&self, text: &str) -> Vec<f32> {
        let mut vector = vec![0.0; self.dim];
        for token in word_tokens(text) {
            self.add_feature(&mut vector, &token, 1.0);
        }
        for gram in char_ngrams(text, 3) {
            self.add_feature(&mut vector, &gram, 0.35);
        }
        l2_normalize(&mut vector);
        vector
    }

    fn dim(&self) -> usize {
        self.dim
    }

    #[allow(clippy::unnecessary_literal_bound)]
    fn name(&self) -> &str {
        "lexical-hash"
    }

    // `is_semantic` is deliberately left to the trait default. The floor is
    // the one backend whose honesty must follow from the default rather than
    // from its own override, so a regression in that default surfaces here
    // instead of hiding behind a local `false`.
}

pub fn default_embedder() -> Arc<dyn Embedder> {
    Arc::new(LexicalEmbedder::default())
}

pub fn cosine(left: &[f32], right: &[f32]) -> f32 {
    if left.is_empty() || left.len() != right.len() {
        return 0.0;
    }
    let mut dot = 0.0;
    let mut left_norm = 0.0;
    let mut right_norm = 0.0;
    for (left, right) in left.iter().zip(right.iter()) {
        if !left.is_finite() || !right.is_finite() {
            return 0.0;
        }
        dot += left * right;
        left_norm += left * left;
        right_norm += right * right;
    }
    if left_norm <= 0.0 || right_norm <= 0.0 {
        return 0.0;
    }
    (dot / (left_norm.sqrt() * right_norm.sqrt())).clamp(-1.0, 1.0)
}

pub fn l2_normalize(vector: &mut [f32]) {
    let norm = vector.iter().map(|value| value * value).sum::<f32>().sqrt();
    if norm > 0.0 {
        for value in vector {
            *value /= norm;
        }
    }
}

pub fn event_search_text(event: &StoredEvent) -> String {
    let mut parts = Vec::new();
    parts.push(event.kind.discriminator().replace('_', " "));
    if let Some(actor) = event.actor.as_deref() {
        parts.push(actor.to_string());
    }
    collect_json_strings(&event.payload, &mut parts);
    parts.join("\n")
}

pub(crate) fn redacted_search_document(
    redactor: Option<&SharedEventRedactor>,
    meta: &SessionMeta,
    event: &StoredEvent,
) -> String {
    redacted_search_document_parts(
        redactor,
        meta.title.as_deref(),
        meta.cwd.as_deref(),
        meta.model.as_deref(),
        meta.project_scope.as_deref(),
        event,
    )
}

pub(crate) fn redacted_search_document_parts(
    redactor: Option<&SharedEventRedactor>,
    title: Option<&str>,
    cwd: Option<&str>,
    model: Option<&str>,
    project_scope: Option<&str>,
    event: &StoredEvent,
) -> String {
    let mut metadata = serde_json::json!({
        "title": title,
        "cwd": cwd,
        "model": model,
        "project_scope": project_scope,
    });
    if let Some(redactor) = redactor {
        redactor.redact_json_in_place(&mut metadata);
    }
    search_document_parts(
        metadata.get("title").and_then(serde_json::Value::as_str),
        metadata.get("cwd").and_then(serde_json::Value::as_str),
        metadata.get("model").and_then(serde_json::Value::as_str),
        metadata
            .get("project_scope")
            .and_then(serde_json::Value::as_str),
        event,
    )
}

pub(crate) fn search_document_parts(
    title: Option<&str>,
    cwd: Option<&str>,
    model: Option<&str>,
    project_scope: Option<&str>,
    event: &StoredEvent,
) -> String {
    let event_text = event_search_text(event);
    [title, cwd, model, project_scope, Some(event_text.as_str())]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join("\n")
}

pub fn snippet(text: &str, query: &str, max_chars: usize) -> String {
    let text = text.trim();
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    let folded = text.to_lowercase();
    let needle = word_tokens(query).into_iter().next().unwrap_or_default();
    let byte_anchor = if needle.is_empty() {
        0
    } else {
        folded.find(&needle).unwrap_or(0)
    };
    let mut original_byte_anchor = byte_anchor.min(text.len());
    while original_byte_anchor > 0 && !text.is_char_boundary(original_byte_anchor) {
        original_byte_anchor -= 1;
    }
    #[expect(
        clippy::string_slice,
        reason = "original_byte_anchor is walked back to a char boundary by the loop above"
    )]
    let char_anchor = text[..original_byte_anchor].chars().count();
    let start = char_anchor.saturating_sub(max_chars / 3);
    let excerpt = text.chars().skip(start).take(max_chars).collect::<String>();
    format!(
        "{}{}{}",
        if start > 0 { "…" } else { "" },
        excerpt,
        if start + max_chars < text.chars().count() {
            "…"
        } else {
            ""
        }
    )
}

pub(crate) fn lexical_score(query: &str, text: &str) -> f32 {
    let query_tokens = word_tokens(query);
    if query_tokens.is_empty() {
        return 0.0;
    }
    let text_tokens = word_tokens(text);
    let frequencies =
        text_tokens
            .into_iter()
            .fold(BTreeMap::<String, usize>::new(), |mut counts, token| {
                *counts.entry(token).or_default() += 1;
                counts
            });
    if query_tokens
        .iter()
        .any(|token| !frequencies.contains_key(token))
    {
        return 0.0;
    }
    let matched = query_tokens
        .iter()
        .filter_map(|token| frequencies.get(token))
        .map(|count| 1.0 + (*count as f32).ln())
        .sum::<f32>();
    let exact = text
        .to_lowercase()
        .contains(query.trim().to_lowercase().as_str());
    matched / query_tokens.len() as f32 + if exact { 1.0 } else { 0.0 }
}

pub(crate) fn combined_score(
    mode: SearchMode,
    fts_rank: Option<usize>,
    semantic_rank: Option<usize>,
    fts_score: Option<f32>,
    semantic_score: Option<f32>,
) -> f32 {
    match mode {
        SearchMode::Fts => fts_score.unwrap_or_default(),
        SearchMode::Semantic => semantic_score.unwrap_or_default(),
        SearchMode::Hybrid => {
            fts_rank
                .map(|rank| 1.0 / (RRF_K + rank as f32 + 1.0))
                .unwrap_or_default()
                + semantic_rank
                    .map(|rank| 1.0 / (RRF_K + rank as f32 + 1.0))
                    .unwrap_or_default()
        }
    }
}

pub(crate) fn ranks(scores: &[f32]) -> BTreeMap<usize, usize> {
    let mut ranked = scores
        .iter()
        .copied()
        .enumerate()
        .filter(|(_, score)| *score > 0.0)
        .collect::<Vec<_>>();
    ranked.sort_by(|(left_index, left), (right_index, right)| {
        right
            .total_cmp(left)
            .then_with(|| left_index.cmp(right_index))
    });
    ranked
        .into_iter()
        .enumerate()
        .map(|(rank, (index, _))| (index, rank))
        .collect()
}

pub(crate) fn vector_blob(vector: &[f32]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(std::mem::size_of_val(vector));
    for value in vector {
        bytes.extend_from_slice(&value.to_le_bytes());
    }
    bytes
}

pub(crate) fn vector_from_blob(bytes: &[u8], dim: usize) -> Option<Vec<f32>> {
    if bytes.len() != dim.checked_mul(std::mem::size_of::<f32>())? {
        return None;
    }
    Some(
        bytes
            .chunks_exact(4)
            .map(|chunk| f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]))
            .collect(),
    )
}

pub(crate) fn word_tokens(text: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut previous_lower = false;
    let flush = |current: &mut String, tokens: &mut Vec<String>| {
        if !current.is_empty() {
            tokens.push(std::mem::take(current));
        }
    };
    for character in text.chars() {
        if character.is_alphanumeric() {
            if character.is_uppercase() && previous_lower {
                flush(&mut current, &mut tokens);
            }
            current.extend(character.to_lowercase());
            previous_lower = character.is_lowercase() || character.is_numeric();
        } else {
            flush(&mut current, &mut tokens);
            previous_lower = false;
        }
    }
    flush(&mut current, &mut tokens);
    tokens
}

pub(crate) fn fts_literal_query(query: &str) -> String {
    word_tokens(query)
        .into_iter()
        .map(|token| format!("\"{}\"", token.replace('"', "\"\"")))
        .collect::<Vec<_>>()
        .join(" AND ")
}

fn char_ngrams(text: &str, width: usize) -> Vec<String> {
    if width == 0 {
        return Vec::new();
    }
    let mut normalized = String::with_capacity(text.len() + 2);
    normalized.push(' ');
    let mut previous_space = true;
    for character in text.chars() {
        if character.is_whitespace() {
            if !previous_space {
                normalized.push(' ');
                previous_space = true;
            }
        } else {
            normalized.extend(character.to_lowercase());
            previous_space = false;
        }
    }
    if !previous_space {
        normalized.push(' ');
    }
    let characters = normalized.chars().collect::<Vec<_>>();
    characters
        .windows(width)
        .map(|window| window.iter().collect())
        .collect()
}

fn fnv1a(bytes: &[u8], seed: u64) -> u64 {
    const FNV_PRIME: u64 = 0x0000_0100_0000_01B3;
    let mut hash = seed ^ 0xcbf2_9ce4_8422_2325;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    hash
}

fn collect_json_strings(value: &serde_json::Value, parts: &mut Vec<String>) {
    match value {
        serde_json::Value::String(text) => parts.push(text.clone()),
        serde_json::Value::Array(items) => {
            for item in items {
                collect_json_strings(item, parts);
            }
        }
        serde_json::Value::Object(fields) => {
            for value in fields.values() {
                collect_json_strings(value, parts);
            }
        }
        serde_json::Value::Null | serde_json::Value::Bool(_) | serde_json::Value::Number(_) => {}
    }
}

/// The measurement behind [`Embedder::is_semantic`].
///
/// `is_semantic` is a claim, and this module is the only place that decides
/// whether the claim holds. Keeping the corpus and the bar here means an
/// embedder in any crate is judged against one definition of "ranks by
/// meaning" rather than against whatever each implementor found convincing.
///
/// The audit is deliberately usable outside this crate's tests: an embedder
/// injected through `StoreHooks` can be held to the same bar by its own
/// author before it ever claims anything.
pub mod conformance {
    use super::{cosine, Embedder};

    /// One retrieval probe.
    ///
    /// `decoy` shares more surface tokens with `query` than `related` does,
    /// so surface matching prefers the decoy and only meaning prefers the
    /// related document. A backend that cannot separate them is ranking by
    /// form, whatever it calls itself.
    pub struct RetrievalCase {
        pub query: &'static str,
        pub related: &'static str,
        pub decoy: &'static str,
    }

    /// The shared corpus. Deliberately domain-generic: these are ordinary
    /// software concepts, not any embedder's or product's vocabulary.
    pub const RETRIEVAL_CASES: &[RetrievalCase] = &[
        RetrievalCase {
            query: "find authentication code",
            related: "verifyCredentials checks the caller identity before granting access",
            decoy: "findCode looks up a numeric status code in a lookup table",
        },
        RetrievalCase {
            query: "where do we log a user in",
            related: "authenticate establishes a session for the account",
            decoy: "logWarning writes a user-facing message to the log file",
        },
        RetrievalCase {
            query: "how are invoices sent to customers",
            related: "deliverReceipt mails the billing document to the account owner",
            decoy: "customerNotes stores free-form text attached to invoices",
        },
        RetrievalCase {
            query: "retry a failed network request",
            related: "backoffScheduler reattempts the call after an exponential delay",
            decoy: "networkRequestLog records every request that failed validation",
        },
        RetrievalCase {
            query: "limit how many requests a client may send",
            related: "throttle rejects traffic above the configured quota",
            decoy: "requestClient sends a limited set of headers with each call",
        },
        RetrievalCase {
            query: "clean up temporary files on shutdown",
            related: "releaseScratchStorage removes working directories when the process exits",
            decoy: "temporaryFileCache keeps a clean list of the files it has opened",
        },
    ];

    /// How many cases a claiming backend must win. One case may be lost, so a
    /// single unlucky probe does not decide the verdict, but a backend that
    /// merely tracks surface form cannot reach it.
    pub fn bar(cases: usize) -> usize {
        cases.saturating_sub(1)
    }

    /// What the audit measured, kept separate from what it concludes so a
    /// caller can report the numbers rather than just a boolean.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct SemanticClaimAudit {
        /// What the backend says about itself.
        pub claims_semantic: bool,
        /// Cases where the related document outscored the decoy.
        pub wins: usize,
        /// Cases probed.
        pub cases: usize,
        /// Whether `wins` reached [`bar`].
        pub clears_bar: bool,
        /// Whether the claim and the measurement agree, in both directions.
        pub honest: bool,
    }

    /// Measure `embedder` against [`RETRIEVAL_CASES`].
    ///
    /// This never panics and never decides policy; it reports. Ties count as
    /// losses, since a backend that cannot separate the pair has not
    /// demonstrated anything.
    pub fn audit_semantic_claim(embedder: &dyn Embedder) -> SemanticClaimAudit {
        let mut wins = 0usize;
        for case in RETRIEVAL_CASES {
            let query = embedder.embed(case.query);
            let related = cosine(&query, &embedder.embed(case.related));
            let decoy = cosine(&query, &embedder.embed(case.decoy));
            if related > decoy {
                wins += 1;
            }
        }
        let cases = RETRIEVAL_CASES.len();
        let clears_bar = wins >= bar(cases);
        let claims_semantic = embedder.is_semantic();
        SemanticClaimAudit {
            claims_semantic,
            wins,
            cases,
            clears_bar,
            honest: claims_semantic == clears_bar,
        }
    }

    /// Assert that `embedder`'s [`Embedder::is_semantic`] answer matches what
    /// it can actually do.
    ///
    /// Both directions are enforced. A backend that claims meaning must clear
    /// the bar, so the claim cannot be aspirational. A backend that disclaims
    /// meaning must fail it, so a genuine upgrade cannot land while callers
    /// are still told they are on the lexical floor.
    pub fn assert_semantic_claim_is_earned(embedder: &dyn Embedder) {
        let audit = audit_semantic_claim(embedder);
        assert!(
            audit.honest,
            "backend `{}` reports is_semantic() == {} but won {}/{} retrieval cases (bar is {}). \
             {}",
            embedder.name(),
            audit.claims_semantic,
            audit.wins,
            audit.cases,
            bar(audit.cases),
            if audit.claims_semantic {
                "The claim is not earned: either fix the backend or drop the override."
            } else {
                "The backend outgrew its disclaimer: override is_semantic() to true so callers \
                 stop being told they are on the lexical floor."
            }
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lexical_embedder_is_deterministic_and_related() {
        let embedder = LexicalEmbedder::default();
        let query = embedder.embed("rate limiting middleware");
        assert_eq!(query, embedder.embed("rate limiting middleware"));
        assert!(
            cosine(&query, &embedder.embed("API rate limiter"))
                > cosine(&query, &embedder.embed("markdown table renderer"))
        );
    }

    #[test]
    fn fts_queries_are_literal_and_identifier_aware() {
        assert_eq!(
            fts_literal_query("getUserByID OR token*"),
            "\"get\" AND \"user\" AND \"by\" AND \"id\" AND \"or\" AND \"token\""
        );
    }

    #[test]
    fn vector_blob_round_trips() {
        let vector = vec![-1.0, 0.25, 4.0];
        assert_eq!(vector_from_blob(&vector_blob(&vector), 3), Some(vector));
        assert_eq!(vector_from_blob(&[0, 1], 3), None);
    }

    #[test]
    fn search_requires_an_explicit_scope() {
        let error = SearchQuery {
            query: "needle".to_string(),
            mode: SearchMode::Fts,
            filter: SearchFilter::default(),
            limit: None,
        }
        .validate()
        .expect_err("unscoped search must be rejected");
        assert!(error.contains("requires"));
    }

    #[test]
    fn unicode_snippet_anchor_never_slices_at_a_folded_byte_offset() {
        let text = format!("{}needle", "İ".repeat(300));
        let rendered = snippet(&text, "needle", 40);
        assert!(rendered.contains("needle"));
    }

    /// Ranks the corpus perfectly by construction. It exists only to prove the
    /// audit can return `honest` for a *claiming* backend, so a green suite is
    /// not just the audit rejecting everything it sees.
    struct OracleEmbedder;

    impl OracleEmbedder {
        /// One-hot per case; query and related share an axis, the decoy gets
        /// its own. Unknown text lands on a spare axis so nothing collides.
        fn axis(text: &str) -> usize {
            let cases = conformance::RETRIEVAL_CASES;
            for (index, case) in cases.iter().enumerate() {
                if case.query == text || case.related == text {
                    return index;
                }
                if case.decoy == text {
                    return cases.len() + index;
                }
            }
            cases.len() * 2
        }
    }

    impl Embedder for OracleEmbedder {
        fn embed(&self, text: &str) -> Vec<f32> {
            let mut vector = vec![0.0; conformance::RETRIEVAL_CASES.len() * 2 + 1];
            vector[Self::axis(text)] = 1.0;
            vector
        }

        fn dim(&self) -> usize {
            conformance::RETRIEVAL_CASES.len() * 2 + 1
        }

        #[allow(clippy::unnecessary_literal_bound)]
        fn name(&self) -> &str {
            "oracle"
        }

        fn is_semantic(&self) -> bool {
            true
        }
    }

    /// The oracle's ranking with the disclaimer left on, so the
    /// under-claiming direction is exercised against a backend that really
    /// can rank rather than against a contrived one.
    struct ModestOracle;

    impl Embedder for ModestOracle {
        fn embed(&self, text: &str) -> Vec<f32> {
            OracleEmbedder.embed(text)
        }

        fn dim(&self) -> usize {
            OracleEmbedder.dim()
        }

        #[allow(clippy::unnecessary_literal_bound)]
        fn name(&self) -> &str {
            "modest-oracle"
        }
    }

    /// Claims meaning, ranks by nothing at all.
    struct BoastfulEmbedder;

    impl Embedder for BoastfulEmbedder {
        fn embed(&self, _text: &str) -> Vec<f32> {
            vec![1.0, 0.0]
        }

        fn dim(&self) -> usize {
            2
        }

        #[allow(clippy::unnecessary_literal_bound)]
        fn name(&self) -> &str {
            "boastful"
        }

        fn is_semantic(&self) -> bool {
            true
        }
    }

    #[test]
    fn lexical_floor_inherits_an_honest_default() {
        // The floor carries no override, so this pins the trait default
        // itself: it must both disclaim and fail the bar.
        let audit = conformance::audit_semantic_claim(&LexicalEmbedder::default());
        assert!(!audit.claims_semantic, "the floor must not claim meaning");
        assert!(
            !audit.clears_bar,
            "surface matching cleared a bar built to defeat it ({}/{}); the corpus has decayed",
            audit.wins, audit.cases
        );
        // Measured at 0/6 when the corpus was written. Held at most 1 so that
        // a decoy quietly losing its surface overlap shows up here, while the
        // gate still has four cases of headroom before it could be at risk.
        assert!(
            audit.wins <= 1,
            "the floor won {}/{} cases; decoys are no longer out-matching the related documents \
             on surface form, so this corpus is drifting toward vacuity",
            audit.wins,
            audit.cases
        );
        conformance::assert_semantic_claim_is_earned(&LexicalEmbedder::default());
    }

    #[test]
    fn a_backend_that_ranks_may_claim() {
        let audit = conformance::audit_semantic_claim(&OracleEmbedder);
        assert_eq!(audit.wins, audit.cases);
        conformance::assert_semantic_claim_is_earned(&OracleEmbedder);
    }

    #[test]
    #[should_panic(expected = "The claim is not earned")]
    fn a_backend_that_cannot_rank_may_not_claim() {
        conformance::assert_semantic_claim_is_earned(&BoastfulEmbedder);
    }

    #[test]
    #[should_panic(expected = "outgrew its disclaimer")]
    fn a_backend_that_outgrows_its_disclaimer_is_caught() {
        conformance::assert_semantic_claim_is_earned(&ModestOracle);
    }
}
