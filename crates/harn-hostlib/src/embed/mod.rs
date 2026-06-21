//! Text-similarity / embedding host capability.
//!
//! A cross-platform, offline, DRY core for cosine/semantic similarity. It
//! is the single source of truth two consumers share:
//!
//! 1. **Push-context Tier-2** (Burin pipelines): auto-injecting
//!    skills/canon/memory/few-shot above a similarity threshold.
//! 2. **`SymbolRelevance`** (Burin Swift): symbol ranking, today split
//!    between macOS-only `NLEmbedding` and a Linux Jaccard fallback. Both
//!    can now route through these builtins for one cross-platform path.
//!
//! ## Surface
//!
//! | Builtin                       | What it does                                              |
//! |-------------------------------|-----------------------------------------------------------|
//! | `hostlib_embed_similarity`    | Cosine similarity of two strings via the active backend.  |
//! | `hostlib_embed_top_k`         | Rank a corpus of strings against a query, return top `k`. |
//! | `hostlib_embed_vector`        | Embed one string to its raw `f32` vector.                 |
//! | `hostlib_embed_info`          | Active backend name + dimensionality.                     |
//!
//! ## Backend selection
//!
//! The capability owns one [`backend::Embedder`] behind an `Arc`, shared
//! across every VM/thread (mirroring `code_index`). Default is the
//! always-available [`backend::LexicalEmbedder`] (zero asset, microsecond,
//! deterministic across OSes). When a Model2Vec/"potion"-style static asset
//! is resolvable (settings/sandbox-aware, no network), the capability
//! upgrades to [`backend::StaticEmbedder`]; if the asset is missing or
//! malformed it degrades cleanly back to lexical. A future candle/ONNX
//! transformer tier slots in behind a Cargo feature without changing this
//! surface or either consumer.

mod backend;
mod similarity;
mod tokenize;

pub use backend::{resolve_asset_dir, Embedder, LexicalEmbedder, StaticEmbedder};
pub use similarity::{cosine, top_k, Scored};

use std::path::Path;
use std::sync::Arc;

use harn_vm::VmValue;

use crate::error::HostlibError;
use crate::registry::{BuiltinRegistry, HostlibCapability, RegisteredBuiltin, SyncHandler};
use crate::tools::args::{build_dict, dict_arg, optional_int, require_string};
use crate::value_args;

/// Builtin name for cosine similarity of two strings.
pub const BUILTIN_SIMILARITY: &str = "hostlib_embed_similarity";
/// Builtin name for top-k corpus ranking against a query.
pub const BUILTIN_TOP_K: &str = "hostlib_embed_top_k";
/// Builtin name for embedding one string to its raw vector.
pub const BUILTIN_VECTOR: &str = "hostlib_embed_vector";
/// Builtin name for reporting the active backend.
pub const BUILTIN_INFO: &str = "hostlib_embed_info";

/// Embedding capability handle. Cloning shares the active backend.
#[derive(Clone)]
pub struct EmbedCapability {
    embedder: Arc<dyn Embedder>,
}

impl Default for EmbedCapability {
    fn default() -> Self {
        Self::lexical()
    }
}

impl EmbedCapability {
    /// Capability using the always-available lexical backend.
    pub fn lexical() -> Self {
        Self {
            embedder: Arc::new(LexicalEmbedder::default()),
        }
    }

    /// Capability backed by an explicit [`Embedder`]. Embedders construct
    /// their own fallback, so this never fails.
    pub fn with_embedder(embedder: Arc<dyn Embedder>) -> Self {
        Self { embedder }
    }

    /// Resolve a static (Model2Vec-style) asset and use it if present,
    /// otherwise fall back to lexical. Sandbox/settings-aware: pass the
    /// override dir (from a setting) and the data dir; nothing else is
    /// touched and there is no network access.
    pub fn resolve(override_dir: Option<&Path>, data_dir: Option<&Path>, model: &str) -> Self {
        if let Some(dir) = resolve_asset_dir(override_dir, data_dir, model) {
            if let Ok(static_embedder) = StaticEmbedder::from_asset_dir(&dir) {
                return Self {
                    embedder: Arc::new(static_embedder),
                };
            }
        }
        Self::lexical()
    }

    /// Borrow the active backend (tests / embedders).
    pub fn embedder(&self) -> &Arc<dyn Embedder> {
        &self.embedder
    }

    fn run_similarity(&self, args: &[VmValue]) -> Result<VmValue, HostlibError> {
        let raw = dict_arg(BUILTIN_SIMILARITY, args)?;
        let dict = raw.as_ref();
        let a = require_string(BUILTIN_SIMILARITY, dict, "a")?;
        let b = require_string(BUILTIN_SIMILARITY, dict, "b")?;
        let va = self.embedder.embed(&a);
        let vb = self.embedder.embed(&b);
        let sim = cosine(&va, &vb);
        // Surface both the raw cosine ([-1,1]) and a clamped relatedness
        // score ([0,1]) so each consumer picks the shape it wants without
        // re-deriving it (DRY: one definition of "relatedness").
        Ok(build_dict([
            ("similarity", VmValue::Float(sim as f64)),
            ("relatedness", VmValue::Float(sim.max(0.0) as f64)),
        ]))
    }

    fn run_top_k(&self, args: &[VmValue]) -> Result<VmValue, HostlibError> {
        let raw = dict_arg(BUILTIN_TOP_K, args)?;
        let dict = raw.as_ref();
        let query = require_string(BUILTIN_TOP_K, dict, "query")?;
        let corpus = require_string_list(BUILTIN_TOP_K, dict, "corpus")?;
        let k = optional_int(BUILTIN_TOP_K, dict, "k", 10)?.max(0) as usize;
        let min_score =
            optional_float(BUILTIN_TOP_K, dict, "min_score")?.unwrap_or(f64::NEG_INFINITY);

        let query_vec = self.embedder.embed(&query);
        let corpus_vecs: Vec<Vec<f32>> = self.embedder.embed_batch(&corpus);
        let ranked = top_k(&query_vec, &corpus_vecs, k);

        let results: Vec<VmValue> = ranked
            .into_iter()
            .filter(|s| (s.score as f64) >= min_score)
            .map(|s| {
                build_dict([
                    ("index", VmValue::Int(s.index as i64)),
                    (
                        "text",
                        VmValue::string(corpus.get(s.index).map(String::as_str).unwrap_or("")),
                    ),
                    ("score", VmValue::Float(s.score as f64)),
                    ("relatedness", VmValue::Float((s.score.max(0.0)) as f64)),
                ])
            })
            .collect();
        Ok(build_dict([("results", VmValue::List(Arc::new(results)))]))
    }

    fn run_vector(&self, args: &[VmValue]) -> Result<VmValue, HostlibError> {
        let raw = dict_arg(BUILTIN_VECTOR, args)?;
        let dict = raw.as_ref();
        let text = require_string(BUILTIN_VECTOR, dict, "text")?;
        let v = self.embedder.embed(&text);
        let values: Vec<VmValue> = v.into_iter().map(|x| VmValue::Float(x as f64)).collect();
        Ok(build_dict([
            ("dim", VmValue::Int(self.embedder.dim() as i64)),
            ("vector", VmValue::List(Arc::new(values))),
        ]))
    }

    fn run_info(&self, _args: &[VmValue]) -> Result<VmValue, HostlibError> {
        Ok(build_dict([
            ("backend", VmValue::string(self.embedder.name())),
            ("dim", VmValue::Int(self.embedder.dim() as i64)),
        ]))
    }
}

impl HostlibCapability for EmbedCapability {
    fn module_name(&self) -> &'static str {
        "embed"
    }

    fn register_builtins(&self, registry: &mut BuiltinRegistry) {
        let cap = self.clone();
        let handler: SyncHandler = Arc::new(move |args| cap.run_similarity(args));
        registry.register(RegisteredBuiltin {
            name: BUILTIN_SIMILARITY,
            module: "embed",
            method: "similarity",
            handler,
        });

        let cap = self.clone();
        let handler: SyncHandler = Arc::new(move |args| cap.run_top_k(args));
        registry.register(RegisteredBuiltin {
            name: BUILTIN_TOP_K,
            module: "embed",
            method: "top_k",
            handler,
        });

        let cap = self.clone();
        let handler: SyncHandler = Arc::new(move |args| cap.run_vector(args));
        registry.register(RegisteredBuiltin {
            name: BUILTIN_VECTOR,
            module: "embed",
            method: "vector",
            handler,
        });

        let cap = self.clone();
        let handler: SyncHandler = Arc::new(move |args| cap.run_info(args));
        registry.register(RegisteredBuiltin {
            name: BUILTIN_INFO,
            module: "embed",
            method: "info",
            handler,
        });
    }
}

// --- local arg helpers (string list + float) -----------------------------

fn require_string_list(
    builtin: &'static str,
    dict: &harn_vm::value::DictMap,
    key: &'static str,
) -> Result<Vec<String>, HostlibError> {
    match value_args::optional_string_list(builtin, dict, key)? {
        Some(list) => Ok(list),
        None => Err(HostlibError::MissingParameter {
            builtin,
            param: key,
        }),
    }
}

fn optional_float(
    builtin: &'static str,
    dict: &harn_vm::value::DictMap,
    key: &'static str,
) -> Result<Option<f64>, HostlibError> {
    match dict.get(key) {
        None | Some(VmValue::Nil) => Ok(None),
        Some(VmValue::Float(f)) => Ok(Some(*f)),
        Some(VmValue::Int(i)) => Ok(Some(*i as f64)),
        Some(other) => Err(HostlibError::InvalidParameter {
            builtin,
            param: key,
            message: format!("expected number, got {}", value_args::describe(other)),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use harn_vm::value::{intern_key, DictMap};

    fn call(cap: &EmbedCapability, builtin: &str, dict: DictMap) -> VmValue {
        let args = [VmValue::dict(dict)];
        match builtin {
            BUILTIN_SIMILARITY => cap.run_similarity(&args).unwrap(),
            BUILTIN_TOP_K => cap.run_top_k(&args).unwrap(),
            BUILTIN_VECTOR => cap.run_vector(&args).unwrap(),
            BUILTIN_INFO => cap.run_info(&args).unwrap(),
            _ => panic!("unknown builtin"),
        }
    }

    fn dict_of(pairs: &[(&str, VmValue)]) -> DictMap {
        let mut m = DictMap::new();
        for (k, v) in pairs {
            m.insert(intern_key(k), v.clone());
        }
        m
    }

    fn get_float(v: &VmValue, key: &str) -> f64 {
        if let VmValue::Dict(d) = v {
            if let Some(VmValue::Float(f)) = d.get(key) {
                return *f;
            }
        }
        panic!("no float {key} in {v:?}");
    }

    fn dict_int(d: &DictMap, key: &str) -> i64 {
        match d.get(key) {
            Some(VmValue::Int(i)) => *i,
            other => panic!("no int {key}: {other:?}"),
        }
    }

    fn dict_str(d: &DictMap, key: &str) -> String {
        match d.get(key) {
            Some(VmValue::String(s)) => s.to_string(),
            other => panic!("no string {key}: {other:?}"),
        }
    }

    #[test]
    fn similarity_self_is_one() {
        let cap = EmbedCapability::lexical();
        let out = call(
            &cap,
            BUILTIN_SIMILARITY,
            dict_of(&[
                ("a", VmValue::string("rate limiter")),
                ("b", VmValue::string("rate limiter")),
            ]),
        );
        assert!((get_float(&out, "similarity") - 1.0).abs() < 1e-5);
        assert!((get_float(&out, "relatedness") - 1.0).abs() < 1e-5);
    }

    #[test]
    fn similarity_relatedness_is_clamped() {
        let cap = EmbedCapability::lexical();
        let out = call(
            &cap,
            BUILTIN_SIMILARITY,
            dict_of(&[
                ("a", VmValue::string("alpha beta gamma")),
                ("b", VmValue::string("delta epsilon zeta")),
            ]),
        );
        // Disjoint text can score slightly negative under signed hashing;
        // relatedness must never be < 0.
        assert!(get_float(&out, "relatedness") >= 0.0);
    }

    #[test]
    fn top_k_ranks_corpus() {
        let cap = EmbedCapability::lexical();
        let out = call(
            &cap,
            BUILTIN_TOP_K,
            dict_of(&[
                ("query", VmValue::string("rate limiter middleware")),
                (
                    "corpus",
                    VmValue::List(Arc::new(vec![
                        VmValue::string("markdown table renderer"),
                        VmValue::string("RateLimiter middleware for the API"),
                        VmValue::string("json parser"),
                    ])),
                ),
                ("k", VmValue::Int(2)),
            ]),
        );
        let VmValue::Dict(d) = &out else { panic!() };
        let VmValue::List(results) = d.get("results").unwrap() else {
            panic!()
        };
        assert_eq!(results.len(), 2);
        // Top hit must be the rate-limiter entry (index 1).
        let VmValue::Dict(first) = &results[0] else {
            panic!()
        };
        assert_eq!(dict_int(first, "index"), 1);
    }

    #[test]
    fn top_k_min_score_filters() {
        let cap = EmbedCapability::lexical();
        let out = call(
            &cap,
            BUILTIN_TOP_K,
            dict_of(&[
                ("query", VmValue::string("rate limiter")),
                (
                    "corpus",
                    VmValue::List(Arc::new(vec![VmValue::string(
                        "completely different topic",
                    )])),
                ),
                ("k", VmValue::Int(5)),
                ("min_score", VmValue::Float(0.99)),
            ]),
        );
        let VmValue::Dict(d) = &out else { panic!() };
        let VmValue::List(results) = d.get("results").unwrap() else {
            panic!()
        };
        assert!(results.is_empty(), "min_score should filter out weak match");
    }

    #[test]
    fn vector_has_declared_dim() {
        let cap = EmbedCapability::lexical();
        let out = call(
            &cap,
            BUILTIN_VECTOR,
            dict_of(&[("text", VmValue::string("hello"))]),
        );
        let VmValue::Dict(d) = &out else { panic!() };
        assert_eq!(dict_int(d, "dim"), 256);
        let VmValue::List(v) = d.get("vector").unwrap() else {
            panic!()
        };
        assert_eq!(v.len(), 256);
    }

    #[test]
    fn info_reports_lexical_default() {
        let cap = EmbedCapability::lexical();
        let out = call(&cap, BUILTIN_INFO, DictMap::new());
        let VmValue::Dict(d) = &out else { panic!() };
        assert_eq!(dict_str(d, "backend"), "lexical-hash");
        assert_eq!(dict_int(d, "dim"), 256);
    }

    #[test]
    fn resolve_degrades_to_lexical_when_absent() {
        let absent = std::env::temp_dir().join("embed-cap-absent-xyz-123");
        let _ = std::fs::remove_dir_all(&absent);
        let cap = EmbedCapability::resolve(Some(&absent), None, "potion");
        assert_eq!(cap.embedder().name(), "lexical-hash");
    }

    #[test]
    fn resolve_uses_static_asset_when_present() {
        let dir = std::env::temp_dir().join("embed-cap-present-xyz-456");
        let _ = std::fs::create_dir_all(&dir);
        std::fs::write(
            dir.join("static-embeddings.json"),
            r#"{ "dim": 2, "vectors": { "rate": [1.0, 0.0], "limit": [0.0, 1.0] } }"#,
        )
        .unwrap();
        let cap = EmbedCapability::resolve(Some(&dir), None, "potion");
        assert_eq!(cap.embedder().name(), "static-model2vec");
        assert_eq!(cap.embedder().dim(), 2);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_required_param_errors() {
        let cap = EmbedCapability::lexical();
        let args = [VmValue::dict(dict_of(&[("a", VmValue::string("x"))]))];
        assert!(matches!(
            cap.run_similarity(&args),
            Err(HostlibError::MissingParameter { param: "b", .. })
        ));
    }

    #[test]
    fn registers_four_builtins() {
        let cap = EmbedCapability::lexical();
        let mut reg = BuiltinRegistry::new();
        cap.register_builtins(&mut reg);
        let names: Vec<_> = reg.iter().map(|b| b.name).collect();
        assert_eq!(
            names,
            vec![
                BUILTIN_SIMILARITY,
                BUILTIN_TOP_K,
                BUILTIN_VECTOR,
                BUILTIN_INFO
            ]
        );
    }
}
