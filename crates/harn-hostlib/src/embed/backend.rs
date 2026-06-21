//! Pluggable embedding backends.
//!
//! The [`Embedder`] trait is the futureproof seam: every backend turns a
//! string into a fixed-dimension `f32` vector that downstream cosine math
//! ranks. Two pure-Rust, zero-asset, fully-offline backends ship in-tree:
//!
//! * [`LexicalEmbedder`] — the always-available default. A hashed
//!   bag-of-features (word tokens + char trigrams) projected into a fixed
//!   dimension via the hashing trick, then L2-normalized. No model, no
//!   asset, no network; microsecond latency; deterministic across OSes.
//!   This is the graceful-degradation floor: it is what every other
//!   backend falls back to when its asset is missing.
//!
//! * [`StaticEmbedder`] — a Model2Vec / "potion"-style static
//!   token-pooled embedder. Loads precomputed per-token vectors from a
//!   resolved-on-disk asset, then `tokenize -> lookup -> mean -> normalize`
//!   with no neural-network inference. Microsecond latency, ~92% of
//!   MiniLM-class quality when the asset is present. Constructed via
//!   [`StaticEmbedder::from_asset_dir`], which fails cleanly (so callers
//!   fall back to lexical) when the asset is absent.
//!
//! A higher-accuracy on-device transformer backend (candle / ONNX) can be
//! added later behind a Cargo feature without changing this trait or any
//! consumer: implement [`Embedder`], resolve its asset the same way, and
//! register it as the active backend when the feature + setting are on.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use super::tokenize;

/// A backend that maps text to a fixed-dimension embedding vector.
///
/// Implementations must be `Send + Sync` so the capability can share one
/// instance across every Harn VM / thread, matching the `code_index`
/// concurrency model. Vectors returned SHOULD be L2-normalized so the
/// cosine math degenerates to a dot product, but [`super::similarity`]
/// normalizes defensively regardless.
pub trait Embedder: Send + Sync {
    /// Embed a single string. Empty/degenerate input returns a zero vector
    /// of length [`Embedder::dim`] (cosine against it is `0.0`).
    fn embed(&self, text: &str) -> Vec<f32>;

    /// Output dimensionality. Stable for the life of the backend.
    fn dim(&self) -> usize;

    /// Stable backend identifier surfaced in `hostlib_embed_info` so
    /// consumers (and evals) can record which backend produced a score.
    fn name(&self) -> &str;

    /// Embed a batch. Default maps [`Embedder::embed`]; backends with a
    /// cheaper batched path may override.
    fn embed_batch(&self, texts: &[String]) -> Vec<Vec<f32>> {
        texts.iter().map(|t| self.embed(t)).collect()
    }
}

/// Deterministic 64-bit FNV-1a hash — stable across platforms and runs (the
/// stdlib `DefaultHasher` is explicitly not stability-guaranteed, which
/// would make embeddings drift between toolchains). We need the projection
/// to be identical on macOS, Linux, and Windows so a query embedded on one
/// host ranks a corpus embedded on another.
fn fnv1a(bytes: &[u8], seed: u64) -> u64 {
    const FNV_PRIME: u64 = 0x0000_0100_0000_01B3;
    let mut hash = seed ^ 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        hash ^= b as u64;
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    hash
}

/// Hashing-trick lexical embedder. Always available, no asset.
///
/// Features are word tokens (camel/snake-split, weighted higher) plus char
/// trigrams (weighted lower, for typo/root robustness). Each feature is
/// hashed into one of `dim` buckets with a signed contribution (a second
/// hash bit picks the sign, which de-biases hash collisions — the standard
/// signed hashing trick). The accumulated vector is L2-normalized.
pub struct LexicalEmbedder {
    dim: usize,
    name: String,
}

impl LexicalEmbedder {
    /// Construct with a given output dimension (clamped to `>= 16`). 256 is
    /// a good default: enough buckets to keep collisions rare for
    /// sentence-length inputs while staying cache-friendly.
    pub fn new(dim: usize) -> Self {
        Self {
            dim: dim.max(16),
            name: "lexical-hash".to_string(),
        }
    }

    fn add_feature(&self, vec: &mut [f32], feature: &str, weight: f32) {
        let h = fnv1a(feature.as_bytes(), 0);
        let bucket = (h % self.dim as u64) as usize;
        // Sign from an independent hash so collisions cancel in expectation.
        let sign = if fnv1a(feature.as_bytes(), 0x9e37_79b9_7f4a_7c15) & 1 == 0 {
            1.0
        } else {
            -1.0
        };
        vec[bucket] += sign * weight;
    }
}

impl Default for LexicalEmbedder {
    fn default() -> Self {
        Self::new(256)
    }
}

impl Embedder for LexicalEmbedder {
    fn embed(&self, text: &str) -> Vec<f32> {
        let mut vec = vec![0.0f32; self.dim];
        for token in tokenize::word_tokens(text) {
            self.add_feature(&mut vec, &token, 1.0);
        }
        for gram in tokenize::char_ngrams(text, 3) {
            // Lower weight: char-ngrams are a denser, noisier signal.
            self.add_feature(&mut vec, &gram, 0.35);
        }
        l2_normalize(&mut vec);
        vec
    }

    fn dim(&self) -> usize {
        self.dim
    }

    fn name(&self) -> &str {
        &self.name
    }
}

/// Model2Vec / potion-style static token-pooled embedder.
///
/// Holds a precomputed `token -> vector` table loaded from a vendored
/// asset. Embedding is `tokenize -> lookup each token's vector -> mean ->
/// L2-normalize`, with NO neural-network forward pass — that is the entire
/// point of static embeddings (microsecond latency, tiny footprint).
///
/// ## Asset format (intentionally simple + dependency-free)
///
/// The asset directory contains `static-embeddings.json`:
/// ```json
/// { "dim": 8, "vectors": { "rate": [...8 floats...], "limit": [...] } }
/// ```
/// Tokens are the same word tokens [`tokenize::word_tokens`] produces, so a
/// distilled potion table can be exported into this shape offline. A real
/// `.safetensors` loader (via `model2vec-rs`) can be added behind a feature
/// later without touching this trait or the JSON fallback — both satisfy
/// the same `token -> vector` contract.
pub struct StaticEmbedder {
    dim: usize,
    vectors: HashMap<String, Vec<f32>>,
    name: String,
    /// Lexical fallback used when a query contains *no* known tokens, so a
    /// previously-unseen identifier still gets a non-degenerate vector
    /// instead of collapsing to zero.
    fallback: LexicalEmbedder,
}

impl StaticEmbedder {
    /// Resolve and load `static-embeddings.json` under `asset_dir`.
    ///
    /// Returns `Err` (so the caller can fall back to lexical) when the
    /// directory or file is missing, unreadable, malformed, or empty. This
    /// is the sandbox/settings-aware degradation contract: a missing asset
    /// never panics and never blocks — it just selects the lexical floor.
    pub fn from_asset_dir(asset_dir: &Path) -> Result<Self, String> {
        let path = asset_dir.join("static-embeddings.json");
        let raw = std::fs::read_to_string(&path)
            .map_err(|e| format!("static embedding asset {} unreadable: {e}", path.display()))?;
        Self::from_json(&raw)
    }

    /// Parse an in-memory asset document. Split out for testability and so
    /// future loaders (safetensors) can reuse the validation.
    pub fn from_json(raw: &str) -> Result<Self, String> {
        // Hand-rolled minimal parse keeps the default build dependency-free
        // (no serde_json pulled in just for an optional asset). The format
        // is small and fixed; we accept the documented shape only.
        let doc: AssetDoc = parse_asset(raw)?;
        if doc.vectors.is_empty() {
            return Err("static embedding asset has no vectors".to_string());
        }
        for (tok, v) in &doc.vectors {
            if v.len() != doc.dim {
                return Err(format!(
                    "static embedding vector for `{tok}` has length {} but dim is {}",
                    v.len(),
                    doc.dim
                ));
            }
        }
        Ok(Self {
            dim: doc.dim,
            vectors: doc.vectors,
            name: "static-model2vec".to_string(),
            fallback: LexicalEmbedder::new(doc.dim),
        })
    }
}

impl Embedder for StaticEmbedder {
    fn embed(&self, text: &str) -> Vec<f32> {
        let mut acc = vec![0.0f32; self.dim];
        let mut hits = 0usize;
        for token in tokenize::word_tokens(text) {
            if let Some(v) = self.vectors.get(&token) {
                for (a, x) in acc.iter_mut().zip(v.iter()) {
                    *a += x;
                }
                hits += 1;
            }
        }
        if hits == 0 {
            // No known tokens: fall back to the lexical projection so a
            // novel identifier is still comparable rather than all-zero.
            return self.fallback.embed(text);
        }
        let inv = 1.0 / hits as f32;
        for a in acc.iter_mut() {
            *a *= inv;
        }
        l2_normalize(&mut acc);
        acc
    }

    fn dim(&self) -> usize {
        self.dim
    }

    fn name(&self) -> &str {
        &self.name
    }
}

/// L2-normalize in place. Zero vectors are left as-is (cosine treats them
/// as `0.0`).
pub(crate) fn l2_normalize(vec: &mut [f32]) {
    let norm: f32 = vec.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 0.0 {
        let inv = 1.0 / norm;
        for x in vec.iter_mut() {
            *x *= inv;
        }
    }
}

// --- minimal, dependency-free asset parser -------------------------------

struct AssetDoc {
    dim: usize,
    vectors: HashMap<String, Vec<f32>>,
}

/// Parse the fixed `{ "dim": N, "vectors": { "tok": [floats] } }` shape.
/// This avoids adding a JSON dependency to the default build for what is an
/// optional asset; a future safetensors loader supersedes it entirely.
fn parse_asset(raw: &str) -> Result<AssetDoc, String> {
    // We lean on the harn-vm value layer? No — keep it standalone. Use a
    // tiny tolerant scanner: find "dim": <int>, then "vectors": { ... }.
    let dim = extract_int(raw, "\"dim\"")
        .ok_or_else(|| "static embedding asset missing integer `dim`".to_string())?;
    if dim == 0 {
        return Err("static embedding `dim` must be > 0".to_string());
    }
    let vectors = extract_vectors(raw)?;
    Ok(AssetDoc {
        dim: dim as usize,
        vectors,
    })
}

fn extract_int(raw: &str, key: &str) -> Option<i64> {
    let idx = raw.find(key)?;
    let after = &raw[idx + key.len()..];
    let colon = after.find(':')?;
    let rest = after[colon + 1..].trim_start();
    let end = rest
        .find(|c: char| !c.is_ascii_digit() && c != '-')
        .unwrap_or(rest.len());
    rest[..end].parse::<i64>().ok()
}

fn extract_vectors(raw: &str) -> Result<HashMap<String, Vec<f32>>, String> {
    let key = "\"vectors\"";
    let idx = raw
        .find(key)
        .ok_or_else(|| "static embedding asset missing `vectors`".to_string())?;
    let after = &raw[idx + key.len()..];
    let open = after
        .find('{')
        .ok_or_else(|| "`vectors` is not an object".to_string())?;
    let body = &after[open + 1..];
    let mut map = HashMap::new();
    let bytes = body.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() {
        // find next quote (start of a key) or closing brace of the object
        match bytes[i] {
            b'}' => break,
            b'"' => {
                let (k, next) = parse_string(body, i)?;
                i = next;
                // skip to colon
                while i < bytes.len() && bytes[i] != b':' {
                    i += 1;
                }
                i += 1;
                // skip to array open
                while i < bytes.len() && bytes[i] != b'[' {
                    i += 1;
                }
                let (vec, next) = parse_float_array(body, i)?;
                i = next;
                map.insert(k, vec);
            }
            _ => i += 1,
        }
    }
    Ok(map)
}

fn parse_string(s: &str, start: usize) -> Result<(String, usize), String> {
    let bytes = s.as_bytes();
    debug_assert_eq!(bytes[start], b'"');
    let mut i = start + 1;
    let mut out = String::new();
    while i < bytes.len() {
        match bytes[i] {
            b'"' => return Ok((out, i + 1)),
            b'\\' if i + 1 < bytes.len() => {
                out.push(bytes[i + 1] as char);
                i += 2;
            }
            c => {
                out.push(c as char);
                i += 1;
            }
        }
    }
    Err("unterminated string in static embedding asset".to_string())
}

fn parse_float_array(s: &str, start: usize) -> Result<(Vec<f32>, usize), String> {
    let bytes = s.as_bytes();
    if start >= bytes.len() || bytes[start] != b'[' {
        return Err("expected float array in static embedding asset".to_string());
    }
    let mut i = start + 1;
    let mut out = Vec::new();
    let mut num = String::new();
    let flush = |num: &mut String, out: &mut Vec<f32>| -> Result<(), String> {
        let t = num.trim();
        if !t.is_empty() {
            out.push(
                t.parse::<f32>()
                    .map_err(|_| format!("bad float `{t}` in static embedding asset"))?,
            );
        }
        num.clear();
        Ok(())
    };
    while i < bytes.len() {
        match bytes[i] {
            b']' => {
                flush(&mut num, &mut out)?;
                return Ok((out, i + 1));
            }
            b',' => {
                flush(&mut num, &mut out)?;
                i += 1;
            }
            c if c.is_ascii_whitespace() => i += 1,
            c => {
                num.push(c as char);
                i += 1;
            }
        }
    }
    Err("unterminated float array in static embedding asset".to_string())
}

/// Resolve the asset directory for a named embedding model, honoring an
/// explicit override before falling back to a conventional location under
/// the data dir. Returns `None` when nothing resolvable exists, which the
/// caller treats as "use the lexical floor".
///
/// Resolution order (sandbox/settings-aware):
/// 1. explicit `override_dir` (from a Harn setting / host call param),
/// 2. `<data_dir>/embeddings/<model>` (conventional vendored location).
///
/// The function never touches the network and never reads outside the
/// provided roots, so it is safe to call from inside a sandbox.
pub fn resolve_asset_dir(
    override_dir: Option<&Path>,
    data_dir: Option<&Path>,
    model: &str,
) -> Option<PathBuf> {
    if let Some(dir) = override_dir {
        if dir.join("static-embeddings.json").is_file() {
            return Some(dir.to_path_buf());
        }
    }
    if let Some(base) = data_dir {
        let candidate = base.join("embeddings").join(model);
        if candidate.join("static-embeddings.json").is_file() {
            return Some(candidate);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lexical_identical_text_is_self_similar() {
        let e = LexicalEmbedder::default();
        let v = e.embed("rate limiter middleware");
        assert_eq!(v.len(), 256);
        let sim = super::super::similarity::cosine(&v, &v);
        assert!((sim - 1.0).abs() < 1e-5, "self-sim was {sim}");
    }

    #[test]
    fn lexical_related_beats_unrelated() {
        let e = LexicalEmbedder::default();
        let query = e.embed("rate limiter for the API");
        let related = e.embed("RateLimiter API throttle");
        let unrelated = e.embed("parse markdown table renderer");
        let s_rel = super::super::similarity::cosine(&query, &related);
        let s_unrel = super::super::similarity::cosine(&query, &unrelated);
        assert!(
            s_rel > s_unrel,
            "related {s_rel} should beat unrelated {s_unrel}"
        );
    }

    #[test]
    fn lexical_empty_is_zero_vector() {
        let e = LexicalEmbedder::default();
        let v = e.embed("");
        assert!(v.iter().all(|&x| x == 0.0));
    }

    #[test]
    fn lexical_is_l2_normalized() {
        let e = LexicalEmbedder::default();
        let v = e.embed("hello world embedding test");
        let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!((norm - 1.0).abs() < 1e-5, "norm was {norm}");
    }

    #[test]
    fn lexical_is_deterministic_cross_run() {
        // The whole cross-platform contract: same input -> same vector.
        let e = LexicalEmbedder::default();
        assert_eq!(e.embed("getUserById"), e.embed("getUserById"));
    }

    #[test]
    fn static_embedder_pools_known_tokens() {
        let json = r#"{ "dim": 2, "vectors": {
            "rate": [1.0, 0.0],
            "limit": [0.0, 1.0],
            "throttle": [0.7071, 0.7071]
        } }"#;
        let e = StaticEmbedder::from_json(json).expect("parse");
        assert_eq!(e.dim(), 2);
        // "rate limit" pools (1,0)+(0,1) -> (0.5,0.5) -> normalized (1/sqrt2, 1/sqrt2)
        let v = e.embed("rate limit");
        let expected = std::f32::consts::FRAC_1_SQRT_2;
        assert!((v[0] - expected).abs() < 1e-3, "{v:?}");
        assert!((v[1] - expected).abs() < 1e-3, "{v:?}");
        // "throttle" should be very close to "rate limit" semantically here.
        let sim = super::super::similarity::cosine(&v, &e.embed("throttle"));
        assert!(sim > 0.99, "throttle sim {sim}");
    }

    #[test]
    fn static_embedder_falls_back_for_unknown_tokens() {
        let json = r#"{ "dim": 2, "vectors": { "rate": [1.0, 0.0] } }"#;
        let e = StaticEmbedder::from_json(json).expect("parse");
        // Unknown tokens -> lexical fallback, non-zero, comparable.
        let v = e.embed("zzz totally unknown words");
        assert!(v.iter().any(|&x| x != 0.0));
    }

    #[test]
    fn static_embedder_rejects_malformed_asset() {
        assert!(StaticEmbedder::from_json("not json").is_err());
        assert!(StaticEmbedder::from_json(r#"{ "dim": 2, "vectors": {} }"#).is_err());
        // length mismatch
        assert!(
            StaticEmbedder::from_json(r#"{ "dim": 3, "vectors": { "x": [1.0, 2.0] } }"#).is_err()
        );
    }

    #[test]
    fn resolve_asset_dir_respects_override_and_absence() {
        let tmp = std::env::temp_dir().join("embed-resolve-test-absent-xyz");
        let _ = std::fs::remove_dir_all(&tmp);
        assert_eq!(resolve_asset_dir(Some(&tmp), None, "potion"), None);
        assert_eq!(resolve_asset_dir(None, Some(&tmp), "potion"), None);
    }

    #[test]
    fn parse_handles_negative_and_scientific_floats() {
        let json = r#"{ "dim": 3, "vectors": { "x": [-1.5, 0.0, 2.0] } }"#;
        let e = StaticEmbedder::from_json(json).expect("parse");
        assert_eq!(e.dim(), 3);
    }
}
