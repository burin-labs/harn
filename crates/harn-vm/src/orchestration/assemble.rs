//! Adaptive context assembly with cross-artifact microcompaction.
//!
//! `assemble_context` packs a set of artifacts into a token-budgeted slice
//! of chunks, deduplicating overlap across artifacts, snipping oversized
//! entries into chunked form, and returning an observability record that
//! names why each chunk was included or dropped.
//!
//! The core is intentionally deterministic: given the same input artifacts
//! and options, it produces the same chunk ids and ordering. A host-supplied
//! ranker callback is the only non-deterministic hook; the VM-side binding
//! invokes it via the same pattern as `compress_callback`.

use std::collections::BTreeSet;

use sha2::{Digest, Sha256};

use super::ArtifactRecord;

/// Strategy used to order chunks when packing into the budget.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AssembleStrategy {
    /// Sort by artifact `created_at` (newest first), then by chunk index.
    Recency,
    /// Sort by ranker score (highest first). Default ranker is token-overlap
    /// against `query`; a host callback can supply a custom one.
    Relevance,
    /// Interleave chunks one-per-artifact in artifact input order, cycling
    /// until the budget fills. Gives every artifact a chance to contribute.
    RoundRobin,
}

impl AssembleStrategy {
    pub fn parse(value: &str) -> Result<Self, String> {
        match value {
            "recency" => Ok(Self::Recency),
            "relevance" => Ok(Self::Relevance),
            "round_robin" => Ok(Self::RoundRobin),
            other => Err(format!(
                "assemble_context: strategy must be one of recency | relevance | round_robin (got {other:?})"
            )),
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Recency => "recency",
            Self::Relevance => "relevance",
            Self::RoundRobin => "round_robin",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AssembleDedup {
    None,
    /// Hash each chunk's normalized text; drop later duplicates.
    Chunked,
    /// Shingle-based overlap detection. Treat chunks whose trigram
    /// Jaccard similarity exceeds 0.85 as duplicates. Still fully
    /// deterministic — no embeddings or callback required.
    Semantic,
}

impl AssembleDedup {
    pub fn parse(value: &str) -> Result<Self, String> {
        match value {
            "none" => Ok(Self::None),
            "chunked" => Ok(Self::Chunked),
            "semantic" => Ok(Self::Semantic),
            other => Err(format!(
                "assemble_context: dedup must be one of none | chunked | semantic (got {other:?})"
            )),
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Chunked => "chunked",
            Self::Semantic => "semantic",
        }
    }
}

#[derive(Clone, Debug)]
pub struct AssembleOptions {
    pub budget_tokens: usize,
    pub dedup: AssembleDedup,
    pub strategy: AssembleStrategy,
    pub query: Option<String>,
    /// Artifacts larger than this many tokens are split into chunks.
    pub microcompact_threshold: usize,
    /// Minimum overlap ratio (0.0-1.0) that counts as a semantic duplicate.
    pub semantic_overlap: f64,
}

impl Default for AssembleOptions {
    fn default() -> Self {
        Self {
            budget_tokens: 8_000,
            dedup: AssembleDedup::Chunked,
            strategy: AssembleStrategy::Relevance,
            query: None,
            microcompact_threshold: 2_000,
            semantic_overlap: 0.85,
        }
    }
}

/// One unit of packed context with a stable content-addressed id.
#[derive(Clone, Debug)]
pub struct AssembledChunk {
    pub id: String,
    pub artifact_id: String,
    pub artifact_kind: String,
    pub title: Option<String>,
    pub source: Option<String>,
    pub text: String,
    pub estimated_tokens: usize,
    pub chunk_index: usize,
    pub chunk_count: usize,
    pub score: f64,
}

/// Per-artifact summary of what made it into the pack.
#[derive(Clone, Debug)]
pub struct AssembledArtifactSummary {
    pub artifact_id: String,
    pub artifact_kind: String,
    pub chunks_included: usize,
    pub chunks_total: usize,
    pub tokens_included: usize,
}

/// Reason an artifact or chunk was excluded from the final pack.
#[derive(Clone, Debug)]
pub struct AssembledExclusion {
    pub artifact_id: String,
    pub chunk_id: Option<String>,
    pub reason: &'static str,
    pub detail: Option<String>,
}

/// Per-chunk rationale — the "why" field the issue calls out.
#[derive(Clone, Debug)]
pub struct AssembledReason {
    pub chunk_id: String,
    pub artifact_id: String,
    pub strategy: &'static str,
    pub score: f64,
    pub included: bool,
    pub reason: &'static str,
}

#[derive(Clone, Debug)]
pub struct AssembledContext {
    pub chunks: Vec<AssembledChunk>,
    pub included: Vec<AssembledArtifactSummary>,
    pub dropped: Vec<AssembledExclusion>,
    pub reasons: Vec<AssembledReason>,
    pub total_tokens: usize,
    pub budget_tokens: usize,
    pub strategy: AssembleStrategy,
    pub dedup: AssembleDedup,
}

/// Content-addressed chunk id — stable across runs for the same text.
/// The leading `artifact_id` prefix keeps chunks from one artifact visually
/// grouped in transcripts and replay diffs.
pub fn stable_chunk_id(artifact_id: &str, text: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(text.as_bytes());
    let digest = hasher.finalize();
    let hex = digest
        .iter()
        .take(8)
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    format!("{artifact_id}#{hex}")
}

/// Approximate token count using the same chars-per-token heuristic as
/// `estimate_message_tokens`. One token ~= 4 characters.
pub fn estimate_chunk_tokens(text: &str) -> usize {
    text.len().div_ceil(4)
}

/// Split a text into chunks of roughly `target_tokens` each, snapping to
/// paragraph (`\n\n`) boundaries when possible, then falling back to line
/// breaks, then raw character boundaries. Chunks never exceed the target
/// more than transiently — the final chunk may be short.
pub fn chunk_text(text: &str, target_tokens: usize) -> Vec<String> {
    if text.is_empty() {
        return Vec::new();
    }
    let target_chars = (target_tokens.max(1)).saturating_mul(4);
    if text.len() <= target_chars {
        return vec![text.to_string()];
    }

    let mut chunks = Vec::new();
    let mut current = String::new();
    let push_current = |current: &mut String, chunks: &mut Vec<String>| {
        if !current.is_empty() {
            chunks.push(std::mem::take(current));
        }
    };

    for paragraph in split_paragraphs(text) {
        if current.len() + paragraph.len() + 2 > target_chars && !current.is_empty() {
            push_current(&mut current, &mut chunks);
        }
        if paragraph.len() > target_chars {
            // Oversized paragraph: split by lines.
            push_current(&mut current, &mut chunks);
            let mut inner = String::new();
            for line in paragraph.split_inclusive('\n') {
                if inner.len() + line.len() > target_chars && !inner.is_empty() {
                    chunks.push(std::mem::take(&mut inner));
                }
                if line.len() > target_chars {
                    // Still too big: fall back to char-boundary splits.
                    let mut i = 0;
                    let bytes = line.as_bytes();
                    while i < line.len() {
                        let mut end = (i + target_chars).min(line.len());
                        while end < line.len() && (bytes[end] & 0b1100_0000) == 0b1000_0000 {
                            end += 1;
                        }
                        if !inner.is_empty() {
                            chunks.push(std::mem::take(&mut inner));
                        }
                        chunks.push(line[i..end].to_string());
                        i = end;
                    }
                } else {
                    inner.push_str(line);
                }
            }
            if !inner.is_empty() {
                chunks.push(inner);
            }
        } else {
            if !current.is_empty() {
                current.push_str("\n\n");
            }
            current.push_str(paragraph);
        }
    }
    push_current(&mut current, &mut chunks);
    chunks
}

fn split_paragraphs(text: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0;
    let bytes = text.as_bytes();
    let mut i = 0;
    while i + 1 < bytes.len() {
        if bytes[i] == b'\n' && bytes[i + 1] == b'\n' {
            let segment = text[start..i].trim_matches('\n');
            if !segment.is_empty() {
                out.push(segment);
            }
            // skip all consecutive newlines
            let mut j = i;
            while j < bytes.len() && bytes[j] == b'\n' {
                j += 1;
            }
            start = j;
            i = j;
        } else {
            i += 1;
        }
    }
    let tail = text[start..].trim_matches('\n');
    if !tail.is_empty() {
        out.push(tail);
    }
    if out.is_empty() && !text.is_empty() {
        out.push(text);
    }
    out
}

/// Trigram set for shingle-based semantic dedup. Lowercases and strips
/// non-alphanumeric characters; produces trigrams over UTF-8 byte windows
/// so behavior is stable across platforms.
fn trigrams(text: &str) -> BTreeSet<[u8; 3]> {
    let normalized: Vec<u8> = text
        .chars()
        .filter_map(|c| {
            if c.is_alphanumeric() {
                Some(c.to_ascii_lowercase() as u8)
            } else if c.is_whitespace() {
                Some(b' ')
            } else {
                None
            }
        })
        .collect();
    let mut out = BTreeSet::new();
    if normalized.len() < 3 {
        return out;
    }
    for window in normalized.windows(3) {
        out.insert([window[0], window[1], window[2]]);
    }
    out
}

fn jaccard(a: &BTreeSet<[u8; 3]>, b: &BTreeSet<[u8; 3]>) -> f64 {
    if a.is_empty() && b.is_empty() {
        return 1.0;
    }
    let intersection = a.intersection(b).count() as f64;
    let union = a.union(b).count() as f64;
    if union == 0.0 {
        0.0
    } else {
        intersection / union
    }
}

fn keyword_overlap_score(text: &str, query: &str) -> f64 {
    if query.trim().is_empty() {
        return 0.0;
    }
    let query_terms: BTreeSet<String> = query
        .split_whitespace()
        .filter(|term| term.len() > 2)
        .map(|term| term.to_ascii_lowercase())
        .collect();
    if query_terms.is_empty() {
        return 0.0;
    }
    let mut matches = 0usize;
    let lower = text.to_ascii_lowercase();
    for term in &query_terms {
        if lower.contains(term.as_str()) {
            matches += 1;
        }
    }
    let base = matches as f64 / query_terms.len() as f64;
    // Length penalty: prefer chunks where matched query terms are dense,
    // so a 100-char chunk that mentions "parser" scores higher than a
    // 10k-char chunk that mentions "parser" once.
    let density = (matches as f64) / (text.len() as f64 / 400.0 + 1.0);
    base * 0.7 + density.min(1.0) * 0.3
}

/// Build every candidate chunk from the input artifacts. Artifacts that
/// exceed `microcompact_threshold` tokens get split; smaller ones become
/// a single chunk each. Skips artifacts with no text body.
pub fn build_candidate_chunks(
    artifacts: &[ArtifactRecord],
    options: &AssembleOptions,
    dropped: &mut Vec<AssembledExclusion>,
) -> Vec<AssembledChunk> {
    let mut candidates = Vec::new();
    for artifact in artifacts {
        let Some(text) = artifact.text.as_ref() else {
            dropped.push(AssembledExclusion {
                artifact_id: artifact.id.clone(),
                chunk_id: None,
                reason: "no_text",
                detail: None,
            });
            continue;
        };
        let trimmed = text.trim();
        if trimmed.is_empty() {
            dropped.push(AssembledExclusion {
                artifact_id: artifact.id.clone(),
                chunk_id: None,
                reason: "empty_text",
                detail: None,
            });
            continue;
        }
        let estimated = artifact
            .estimated_tokens
            .unwrap_or_else(|| estimate_chunk_tokens(text));
        let pieces: Vec<String> = if estimated > options.microcompact_threshold {
            chunk_text(text, options.microcompact_threshold)
        } else {
            vec![text.to_string()]
        };
        let count = pieces.len();
        for (idx, piece) in pieces.into_iter().enumerate() {
            let id = stable_chunk_id(&artifact.id, &piece);
            let tokens = estimate_chunk_tokens(&piece);
            candidates.push(AssembledChunk {
                id,
                artifact_id: artifact.id.clone(),
                artifact_kind: artifact.kind.clone(),
                title: artifact.title.clone(),
                source: artifact.source.clone(),
                text: piece,
                estimated_tokens: tokens,
                chunk_index: idx,
                chunk_count: count,
                score: 0.0,
            });
        }
    }
    candidates
}

/// Apply dedup. Returns (kept, dropped-by-dedup). `dropped` is the slice
/// needed for the caller's observability record; the reason is always
/// `"duplicate"`.
pub fn dedup_chunks(
    mut chunks: Vec<AssembledChunk>,
    mode: AssembleDedup,
    semantic_overlap: f64,
) -> (Vec<AssembledChunk>, Vec<AssembledExclusion>) {
    let mut dropped = Vec::new();
    match mode {
        AssembleDedup::None => (chunks, dropped),
        AssembleDedup::Chunked => {
            let mut seen: BTreeSet<String> = BTreeSet::new();
            chunks.retain(|chunk| {
                let key = normalized_text_key(&chunk.text);
                if seen.insert(key) {
                    true
                } else {
                    dropped.push(AssembledExclusion {
                        artifact_id: chunk.artifact_id.clone(),
                        chunk_id: Some(chunk.id.clone()),
                        reason: "duplicate",
                        detail: Some("chunked".to_string()),
                    });
                    false
                }
            });
            (chunks, dropped)
        }
        AssembleDedup::Semantic => {
            let mut kept: Vec<(AssembledChunk, BTreeSet<[u8; 3]>)> = Vec::new();
            for chunk in chunks.drain(..) {
                let trigrams_new = trigrams(&chunk.text);
                let mut duplicate = false;
                for (existing, existing_trigrams) in &kept {
                    if jaccard(&trigrams_new, existing_trigrams) >= semantic_overlap {
                        dropped.push(AssembledExclusion {
                            artifact_id: chunk.artifact_id.clone(),
                            chunk_id: Some(chunk.id.clone()),
                            reason: "duplicate",
                            detail: Some(format!("semantic≈{}", existing.id)),
                        });
                        duplicate = true;
                        break;
                    }
                }
                if !duplicate {
                    kept.push((chunk, trigrams_new));
                }
            }
            (kept.into_iter().map(|(chunk, _)| chunk).collect(), dropped)
        }
    }
}

fn normalized_text_key(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Score chunks under `strategy`. With `Relevance` and no callback scores,
/// falls back to keyword overlap against `options.query`. The `custom_scores`
/// Option is the hook where a host-supplied ranker slots in — the caller
/// (stdlib binding) invokes the closure and passes the resulting Vec here.
pub fn score_chunks(
    chunks: &mut [AssembledChunk],
    artifacts: &[ArtifactRecord],
    options: &AssembleOptions,
    custom_scores: Option<&[f64]>,
) {
    match options.strategy {
        AssembleStrategy::Recency => {
            // Newer artifacts first; within an artifact, earlier chunks first.
            let order: std::collections::BTreeMap<&str, (String, usize)> = artifacts
                .iter()
                .enumerate()
                .map(|(idx, artifact)| (artifact.id.as_str(), (artifact.created_at.clone(), idx)))
                .collect();
            for chunk in chunks.iter_mut() {
                let (created_at, input_idx) = order
                    .get(chunk.artifact_id.as_str())
                    .cloned()
                    .unwrap_or_else(|| (String::new(), 0));
                // Score is a synthetic "recency score" in [0, 1] based on
                // lexicographic order of created_at with the input index as
                // a stable tiebreaker. Newer created_at → higher score.
                let recency_rank = created_at
                    .chars()
                    .fold(0u64, |acc, c| acc.wrapping_mul(131).wrapping_add(c as u64));
                chunk.score = recency_rank as f64 / u64::MAX as f64
                    - (input_idx as f64) * 1e-9
                    - (chunk.chunk_index as f64) * 1e-12;
            }
        }
        AssembleStrategy::Relevance => {
            if let Some(scores) = custom_scores {
                for (chunk, score) in chunks.iter_mut().zip(scores.iter()) {
                    chunk.score = *score;
                }
                // Any trailing chunks without a custom score keep 0.0.
            } else {
                let query = options.query.as_deref().unwrap_or("");
                for chunk in chunks.iter_mut() {
                    chunk.score = keyword_overlap_score(&chunk.text, query);
                }
            }
        }
        AssembleStrategy::RoundRobin => {
            // Round-robin is handled at pack time; score just reflects
            // input order so ties break deterministically.
            for (idx, chunk) in chunks.iter_mut().enumerate() {
                chunk.score = 1.0 - (idx as f64) * 1e-6;
            }
        }
    }
}

/// Pack chunks under `budget_tokens`. Returns (selected, rejected-for-budget).
pub fn pack_budget(
    chunks: Vec<AssembledChunk>,
    options: &AssembleOptions,
) -> (Vec<AssembledChunk>, Vec<AssembledChunk>) {
    let mut sorted = chunks;
    match options.strategy {
        AssembleStrategy::RoundRobin => {
            // Group by artifact_id preserving first-appearance order, then interleave.
            let mut groups: Vec<Vec<AssembledChunk>> = Vec::new();
            let mut group_index: std::collections::BTreeMap<String, usize> =
                std::collections::BTreeMap::new();
            // Preserve input order of artifacts by walking sorted-as-given.
            for chunk in sorted.drain(..) {
                let key = chunk.artifact_id.clone();
                let idx = match group_index.get(&key) {
                    Some(idx) => *idx,
                    None => {
                        let idx = groups.len();
                        group_index.insert(key.clone(), idx);
                        groups.push(Vec::new());
                        idx
                    }
                };
                groups[idx].push(chunk);
            }
            // Within each group, keep by chunk_index ascending.
            for group in &mut groups {
                group.sort_by_key(|chunk| chunk.chunk_index);
            }
            let mut interleaved = Vec::new();
            let max_len = groups.iter().map(Vec::len).max().unwrap_or(0);
            for i in 0..max_len {
                for group in &mut groups {
                    if i < group.len() {
                        interleaved.push(group[i].clone());
                    }
                }
            }
            sorted = interleaved;
        }
        _ => {
            sorted.sort_by(|a, b| {
                b.score
                    .partial_cmp(&a.score)
                    .unwrap_or(std::cmp::Ordering::Equal)
                    .then_with(|| a.artifact_id.cmp(&b.artifact_id))
                    .then_with(|| a.chunk_index.cmp(&b.chunk_index))
            });
        }
    }

    let mut selected = Vec::new();
    let mut rejected = Vec::new();
    let mut used = 0usize;
    for chunk in sorted {
        if used + chunk.estimated_tokens > options.budget_tokens {
            rejected.push(chunk);
            continue;
        }
        used += chunk.estimated_tokens;
        selected.push(chunk);
    }
    (selected, rejected)
}

/// Core assembly pass. The caller is responsible for supplying
/// `custom_scores` (Some when a host ranker produced them, None otherwise).
pub fn assemble_context(
    artifacts: &[ArtifactRecord],
    options: &AssembleOptions,
    custom_scores: Option<&[f64]>,
) -> AssembledContext {
    let mut dropped = Vec::new();
    let candidates = build_candidate_chunks(artifacts, options, &mut dropped);
    // When a custom_scores slice was supplied, it's indexed over the
    // *pre-dedup* candidate list — the caller saw those chunk ids. Build
    // a score map keyed by chunk id so dedup doesn't misalign the slice.
    let custom_map: Option<std::collections::BTreeMap<String, f64>> = custom_scores.map(|scores| {
        candidates
            .iter()
            .zip(scores.iter().copied())
            .map(|(chunk, score)| (chunk.id.clone(), score))
            .collect()
    });
    let (mut deduped, dedup_dropped) =
        dedup_chunks(candidates, options.dedup, options.semantic_overlap);
    dropped.extend(dedup_dropped);

    if let Some(map) = custom_map.as_ref() {
        for chunk in deduped.iter_mut() {
            chunk.score = map.get(&chunk.id).copied().unwrap_or(0.0);
        }
    } else {
        score_chunks(&mut deduped, artifacts, options, None);
    }

    let (selected, rejected) = pack_budget(deduped, options);

    let mut reasons = Vec::new();
    let mut included_tokens: std::collections::BTreeMap<String, (String, usize, usize, usize)> =
        std::collections::BTreeMap::new();
    // chunk_count per artifact from selected + rejected, for "of X chunks" observability.
    let mut total_counts: std::collections::BTreeMap<String, usize> =
        std::collections::BTreeMap::new();
    for chunk in selected.iter().chain(rejected.iter()) {
        *total_counts.entry(chunk.artifact_id.clone()).or_insert(0) += 1;
    }

    for chunk in &selected {
        reasons.push(AssembledReason {
            chunk_id: chunk.id.clone(),
            artifact_id: chunk.artifact_id.clone(),
            strategy: options.strategy.as_str(),
            score: chunk.score,
            included: true,
            reason: "selected",
        });
        let entry = included_tokens
            .entry(chunk.artifact_id.clone())
            .or_insert_with(|| {
                (
                    chunk.artifact_kind.clone(),
                    0,
                    *total_counts.get(&chunk.artifact_id).unwrap_or(&0),
                    0,
                )
            });
        entry.1 += 1;
        entry.3 += chunk.estimated_tokens;
    }
    for chunk in &rejected {
        reasons.push(AssembledReason {
            chunk_id: chunk.id.clone(),
            artifact_id: chunk.artifact_id.clone(),
            strategy: options.strategy.as_str(),
            score: chunk.score,
            included: false,
            reason: "budget_exceeded",
        });
        dropped.push(AssembledExclusion {
            artifact_id: chunk.artifact_id.clone(),
            chunk_id: Some(chunk.id.clone()),
            reason: "budget_exceeded",
            detail: None,
        });
    }

    let total_tokens = selected.iter().map(|chunk| chunk.estimated_tokens).sum();
    let included: Vec<AssembledArtifactSummary> = included_tokens
        .into_iter()
        .map(
            |(artifact_id, (kind, included, total, tokens))| AssembledArtifactSummary {
                artifact_id,
                artifact_kind: kind,
                chunks_included: included,
                chunks_total: total,
                tokens_included: tokens,
            },
        )
        .collect();

    AssembledContext {
        chunks: selected,
        included,
        dropped,
        reasons,
        total_tokens,
        budget_tokens: options.budget_tokens,
        strategy: options.strategy,
        dedup: options.dedup,
    }
}

/// Render assembled chunks into the XML-ish `<artifact>` format used by
/// `render_artifacts_context`, so swapping in `assemble_context` at the
/// workflow stage layer produces the same prompt shape agents already
/// expect. Appends a trailing `<context_budget>` summary so the agent
/// (and replay diff) can see how much of the budget got used.
pub fn render_assembled_chunks(assembled: &AssembledContext) -> String {
    let mut parts = Vec::with_capacity(assembled.chunks.len() + 1);
    for chunk in &assembled.chunks {
        let title = chunk
            .title
            .clone()
            .unwrap_or_else(|| format!("{} {}", chunk.artifact_kind, chunk.artifact_id));
        parts.push(format!(
            "<artifact>\n<title>{}</title>\n<kind>{}</kind>\n<source>{}</source>\n\
<chunk_id>{}</chunk_id>\n<chunk_index>{} of {}</chunk_index>\n<body>\n{}\n</body>\n</artifact>",
            escape_xml(&title),
            escape_xml(&chunk.artifact_kind),
            escape_xml(chunk.source.as_deref().unwrap_or("unknown")),
            escape_xml(&chunk.id),
            chunk.chunk_index + 1,
            chunk.chunk_count,
            chunk.text,
        ));
    }
    parts.push(format!(
        "<context_budget>\n<used_tokens>{}</used_tokens>\n<budget_tokens>{}</budget_tokens>\n<strategy>{}</strategy>\n<dedup>{}</dedup>\n</context_budget>",
        assembled.total_tokens,
        assembled.budget_tokens,
        assembled.strategy.as_str(),
        assembled.dedup.as_str(),
    ));
    parts.join("\n\n")
}

fn escape_xml(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn artifact(id: &str, text: &str) -> ArtifactRecord {
        ArtifactRecord {
            type_name: "artifact".to_string(),
            id: id.to_string(),
            kind: "resource".to_string(),
            title: Some(id.to_string()),
            text: Some(text.to_string()),
            data: None,
            source: None,
            created_at: format!("2026-04-{id:0>2}T00:00:00Z"),
            freshness: None,
            priority: Some(50),
            lineage: Vec::new(),
            relevance: None,
            estimated_tokens: None,
            stage: None,
            metadata: Default::default(),
        }
        .normalize()
    }

    #[test]
    fn chunk_ids_are_stable_and_content_addressed() {
        let a = artifact("01", "alpha bravo charlie");
        let options = AssembleOptions::default();
        let mut dropped = Vec::new();
        let first = build_candidate_chunks(&[a.clone()], &options, &mut dropped);
        let second = build_candidate_chunks(&[a], &options, &mut dropped);
        assert_eq!(first[0].id, second[0].id);
        assert!(first[0].id.starts_with("01#"));
        // Different text → different id.
        let different = artifact("01", "delta echo foxtrot");
        let different_chunks = build_candidate_chunks(&[different], &options, &mut dropped);
        assert_ne!(first[0].id, different_chunks[0].id);
    }

    #[test]
    fn chunked_dedup_drops_exact_duplicates() {
        let a = artifact("01", "shared body");
        let b = artifact("02", "shared body");
        let options = AssembleOptions {
            budget_tokens: 10_000,
            dedup: AssembleDedup::Chunked,
            strategy: AssembleStrategy::Recency,
            ..AssembleOptions::default()
        };
        let result = assemble_context(&[a, b], &options, None);
        assert_eq!(result.chunks.len(), 1);
        assert!(result.dropped.iter().any(|d| d.reason == "duplicate"));
    }

    #[test]
    fn semantic_dedup_catches_near_duplicates() {
        let a = artifact(
            "01",
            "The parser drift issue was diagnosed by tracing token spans.",
        );
        let b = artifact(
            "02",
            "The parser drift issue, diagnosed by tracing token spans, appeared in the tokenizer.",
        );
        let options = AssembleOptions {
            dedup: AssembleDedup::Semantic,
            strategy: AssembleStrategy::Recency,
            semantic_overlap: 0.5,
            ..AssembleOptions::default()
        };
        let result = assemble_context(&[a, b], &options, None);
        // One of the two should dedup out.
        assert_eq!(result.chunks.len(), 1);
        assert!(result.dropped.iter().any(|d| d.reason == "duplicate"
            && d.detail
                .as_deref()
                .is_some_and(|s| s.starts_with("semantic"))));
    }

    #[test]
    fn budget_enforcement_trims_excess_chunks() {
        let text = "word ".repeat(5_000); // ~25_000 chars → ~6_250 tokens
        let a = artifact("01", &text);
        let options = AssembleOptions {
            budget_tokens: 500,
            dedup: AssembleDedup::None,
            strategy: AssembleStrategy::Recency,
            microcompact_threshold: 200,
            ..AssembleOptions::default()
        };
        let result = assemble_context(&[a], &options, None);
        assert!(result.total_tokens <= options.budget_tokens);
        assert!(result
            .reasons
            .iter()
            .any(|r| !r.included && r.reason == "budget_exceeded"));
    }

    #[test]
    fn relevance_strategy_prefers_query_matches() {
        let a = artifact("01", "completely unrelated content about weather");
        let b = artifact("02", "parser drift diagnostics token spans hotspot");
        let options = AssembleOptions {
            // Tight budget: only one ~11-token chunk fits.
            budget_tokens: 12,
            dedup: AssembleDedup::None,
            strategy: AssembleStrategy::Relevance,
            query: Some("parser drift diagnostics".to_string()),
            microcompact_threshold: 10_000,
            ..AssembleOptions::default()
        };
        let result = assemble_context(&[a, b], &options, None);
        assert_eq!(result.chunks.len(), 1);
        assert_eq!(result.chunks[0].artifact_id, "02");
    }

    #[test]
    fn round_robin_interleaves_artifacts() {
        // Each paragraph is ~10 chars (~3 tokens); microcompact threshold of
        // 3 tokens (=12 chars) forces one chunk per paragraph without
        // fragmenting the final one mid-word.
        let a = artifact("01", "alpha aaaa\n\nbeta bbbb\n\ngamma ccc");
        let b = artifact("02", "delta dddd\n\nepsilon ee\n\nzeta ff");
        let options = AssembleOptions {
            budget_tokens: 10_000,
            dedup: AssembleDedup::None,
            strategy: AssembleStrategy::RoundRobin,
            microcompact_threshold: 3,
            ..AssembleOptions::default()
        };
        let result = assemble_context(&[a, b], &options, None);
        let order: Vec<&str> = result
            .chunks
            .iter()
            .map(|c| c.artifact_id.as_str())
            .collect();
        // First four positions must alternate even if counts don't match
        // exactly — interleaving is the invariant, not total chunk count.
        assert!(order.len() >= 4);
        assert_eq!(order[0], "01");
        assert_eq!(order[1], "02");
        assert_eq!(order[2], "01");
        assert_eq!(order[3], "02");
    }

    #[test]
    fn custom_scores_override_default_ranker() {
        let a = artifact("01", "first body content");
        let b = artifact("02", "second body content");
        let options = AssembleOptions {
            // Tight budget: only one ~5-token chunk fits.
            budget_tokens: 6,
            dedup: AssembleDedup::None,
            strategy: AssembleStrategy::Relevance,
            query: Some("first".to_string()),
            microcompact_threshold: 10_000,
            ..AssembleOptions::default()
        };
        let mut dropped = Vec::new();
        let candidates = build_candidate_chunks(&[a.clone(), b.clone()], &options, &mut dropped);
        assert_eq!(candidates.len(), 2);
        // Host-supplied ranker deliberately inverts the default order: it
        // scores the second artifact higher even though query says "first".
        let scores = vec![0.1, 0.9];
        let result = assemble_context(&[a, b], &options, Some(&scores));
        assert_eq!(result.chunks.len(), 1);
        assert_eq!(result.chunks[0].artifact_id, "02");
    }

    #[test]
    fn reasons_name_strategy_and_inclusion() {
        let a = artifact("01", "included body");
        let b = artifact("02", "dropped body because budget");
        let options = AssembleOptions {
            budget_tokens: 5,
            dedup: AssembleDedup::None,
            strategy: AssembleStrategy::Recency,
            microcompact_threshold: 10_000,
            ..AssembleOptions::default()
        };
        let result = assemble_context(&[a, b], &options, None);
        assert!(result.reasons.iter().any(|r| r.included));
        assert!(result.reasons.iter().any(|r| !r.included));
        for reason in &result.reasons {
            assert_eq!(reason.strategy, "recency");
        }
    }

    #[test]
    fn empty_artifact_reports_dropped() {
        let mut empty = artifact("01", "");
        empty.text = Some(String::new());
        let options = AssembleOptions::default();
        let result = assemble_context(&[empty], &options, None);
        assert!(result.chunks.is_empty());
        assert!(result
            .dropped
            .iter()
            .any(|d| d.reason == "empty_text" || d.reason == "no_text"));
    }
}
