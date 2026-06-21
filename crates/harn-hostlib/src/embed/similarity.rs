//! Pure, backend-agnostic vector math: cosine similarity and top-k.
//!
//! This is the single source of truth both Swift (`SymbolRelevance`) and
//! Harn pipelines (push-context Tier-2 skill/canon/memory injection) call
//! through. Keeping the math here — rather than duplicated in each consumer
//! — is the whole point of the DRY embedding core: one ranking definition,
//! one set of edge-case decisions (empty corpus, zero vectors, NaN guards).

/// Cosine similarity between two equal-length vectors.
///
/// Returns a value in `[-1.0, 1.0]`. Degenerate inputs (mismatched length,
/// either side a zero vector, or any non-finite component) return `0.0`
/// rather than `NaN` so downstream ranking is always well-ordered. Callers
/// that want a `[0, 1]` "relatedness" score should `max(0.0, sim)`.
pub fn cosine(a: &[f32], b: &[f32]) -> f32 {
    if a.is_empty() || a.len() != b.len() {
        return 0.0;
    }
    let mut dot = 0.0f32;
    let mut norm_a = 0.0f32;
    let mut norm_b = 0.0f32;
    for (x, y) in a.iter().zip(b.iter()) {
        dot += x * y;
        norm_a += x * x;
        norm_b += y * y;
    }
    if norm_a <= 0.0 || norm_b <= 0.0 {
        return 0.0;
    }
    let denom = (norm_a.sqrt()) * (norm_b.sqrt());
    if denom <= 0.0 {
        return 0.0;
    }
    let sim = dot / denom;
    if sim.is_finite() {
        // Clamp tiny floating-point overshoots back into range.
        sim.clamp(-1.0, 1.0)
    } else {
        0.0
    }
}

/// One scored corpus entry, returned by [`top_k`].
#[derive(Debug, Clone, PartialEq)]
pub struct Scored {
    /// Index into the original corpus slice.
    pub index: usize,
    /// Cosine similarity of this entry's vector against the query.
    pub score: f32,
}

/// Rank `corpus` vectors against `query` and return the top `k` by cosine
/// similarity, highest first.
///
/// Ties break by ascending index so the ordering is deterministic (matters
/// for reproducible evals and snapshot tests). `k == 0` or an empty corpus
/// returns an empty vector. Entries whose vector length does not match the
/// query score `0.0` (via [`cosine`]) rather than being silently dropped,
/// so the result length is stable.
pub fn top_k(query: &[f32], corpus: &[Vec<f32>], k: usize) -> Vec<Scored> {
    if k == 0 || corpus.is_empty() {
        return Vec::new();
    }
    let mut scored: Vec<Scored> = corpus
        .iter()
        .enumerate()
        .map(|(index, vec)| Scored {
            index,
            score: cosine(query, vec),
        })
        .collect();
    // Sort by score desc, then index asc for determinism. `total_cmp`
    // keeps the comparator total even if a stray non-finite slips in.
    scored.sort_by(|a, b| {
        b.score
            .total_cmp(&a.score)
            .then_with(|| a.index.cmp(&b.index))
    });
    scored.truncate(k);
    scored
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cosine_identical_vectors_is_one() {
        let v = vec![1.0, 2.0, 3.0];
        assert!((cosine(&v, &v) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn cosine_orthogonal_is_zero() {
        assert!(cosine(&[1.0, 0.0], &[0.0, 1.0]).abs() < 1e-6);
    }

    #[test]
    fn cosine_opposite_is_negative_one() {
        assert!((cosine(&[1.0, 0.0], &[-1.0, 0.0]) + 1.0).abs() < 1e-6);
    }

    #[test]
    fn cosine_handles_degenerate_inputs() {
        assert_eq!(cosine(&[], &[]), 0.0);
        assert_eq!(cosine(&[1.0, 2.0], &[1.0]), 0.0); // length mismatch
        assert_eq!(cosine(&[0.0, 0.0], &[1.0, 1.0]), 0.0); // zero vector
        assert_eq!(cosine(&[f32::NAN, 1.0], &[1.0, 1.0]), 0.0); // non-finite
    }

    #[test]
    fn top_k_ranks_highest_first() {
        let query = vec![1.0, 0.0];
        let corpus = vec![
            vec![0.0, 1.0],  // orthogonal -> 0
            vec![1.0, 0.0],  // identical -> 1
            vec![1.0, 1.0],  // 45deg -> ~0.707
            vec![-1.0, 0.0], // opposite -> -1
        ];
        let ranked = top_k(&query, &corpus, 3);
        assert_eq!(ranked.len(), 3);
        assert_eq!(ranked[0].index, 1);
        assert_eq!(ranked[1].index, 2);
        assert_eq!(ranked[2].index, 0);
    }

    #[test]
    fn top_k_is_deterministic_on_ties() {
        let query = vec![1.0, 0.0];
        // Three identical vectors -> all tie at 1.0; must come back 0,1,2.
        let corpus = vec![vec![1.0, 0.0], vec![1.0, 0.0], vec![1.0, 0.0]];
        let ranked = top_k(&query, &corpus, 3);
        assert_eq!(
            ranked.iter().map(|s| s.index).collect::<Vec<_>>(),
            vec![0, 1, 2]
        );
    }

    #[test]
    fn top_k_empty_and_zero_k() {
        assert!(top_k(&[1.0], &[], 5).is_empty());
        assert!(top_k(&[1.0], &[vec![1.0]], 0).is_empty());
    }

    #[test]
    fn top_k_clamps_to_corpus_size() {
        let ranked = top_k(&[1.0], &[vec![1.0], vec![0.5]], 100);
        assert_eq!(ranked.len(), 2);
    }
}
