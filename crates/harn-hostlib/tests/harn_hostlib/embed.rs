//! Integration tests for the `embed` host capability.
//!
//! Exercises the capability through its registered builtins (the surface
//! Harn pipelines and the Swift bridge see), covering cosine correctness,
//! top-k ranking, degenerate inputs, and the sandbox/asset-absent
//! degradation contract.

use std::sync::Arc;

use harn_hostlib::embed::EmbedCapability;
use harn_hostlib::{BuiltinRegistry, HostlibCapability, RegisteredBuiltin};
use harn_vm::value::{intern_key, DictMap};
use harn_vm::VmValue;

fn registry_for(cap: EmbedCapability) -> BuiltinRegistry {
    let mut reg = BuiltinRegistry::new();
    cap.register_builtins(&mut reg);
    reg
}

fn invoke(reg: &BuiltinRegistry, name: &str, pairs: &[(&str, VmValue)]) -> VmValue {
    let mut m = DictMap::new();
    for (k, v) in pairs {
        m.insert(intern_key(k), v.clone());
    }
    let b: &RegisteredBuiltin = reg.find(name).expect("builtin registered");
    (b.handler)(&[VmValue::dict(m)]).expect("builtin ran")
}

fn dict_get<'a>(v: &'a VmValue, key: &str) -> &'a VmValue {
    let VmValue::Dict(d) = v else {
        panic!("not a dict: {v:?}")
    };
    d.get(key).unwrap_or_else(|| panic!("no key {key}"))
}

fn as_f64(v: &VmValue) -> f64 {
    match v {
        VmValue::Float(f) => *f,
        VmValue::Int(i) => *i as f64,
        other => panic!("not a number: {other:?}"),
    }
}

#[test]
fn similarity_identical_is_one_and_relatedness_clamped() {
    let reg = registry_for(EmbedCapability::lexical());
    let out = invoke(
        &reg,
        "hostlib_embed_similarity",
        &[
            ("a", VmValue::string("retry backoff handler")),
            ("b", VmValue::string("retry backoff handler")),
        ],
    );
    assert!((as_f64(dict_get(&out, "similarity")) - 1.0).abs() < 1e-5);
    assert!(as_f64(dict_get(&out, "relatedness")) >= 0.0);
}

#[test]
fn similarity_related_beats_unrelated() {
    let reg = registry_for(EmbedCapability::lexical());
    let related = invoke(
        &reg,
        "hostlib_embed_similarity",
        &[
            ("a", VmValue::string("rate limiter middleware")),
            (
                "b",
                VmValue::string("RateLimiterMiddleware throttle requests"),
            ),
        ],
    );
    let unrelated = invoke(
        &reg,
        "hostlib_embed_similarity",
        &[
            ("a", VmValue::string("rate limiter middleware")),
            ("b", VmValue::string("markdown table of contents renderer")),
        ],
    );
    assert!(as_f64(dict_get(&related, "similarity")) > as_f64(dict_get(&unrelated, "similarity")));
}

#[test]
fn top_k_ranks_and_respects_k() {
    let reg = registry_for(EmbedCapability::lexical());
    let out = invoke(
        &reg,
        "hostlib_embed_top_k",
        &[
            ("query", VmValue::string("authentication token validation")),
            (
                "corpus",
                VmValue::List(Arc::new(vec![
                    VmValue::string("markdown renderer"),
                    VmValue::string("validate the auth token on each request"),
                    VmValue::string("json parser"),
                    VmValue::string("auth token refresh and validation flow"),
                ])),
            ),
            ("k", VmValue::Int(2)),
        ],
    );
    let VmValue::List(results) = dict_get(&out, "results") else {
        panic!()
    };
    assert_eq!(results.len(), 2);
    // The two auth-token entries (index 1 and 3) should rank above the
    // unrelated markdown/json entries.
    let top_indices: Vec<i64> = results
        .iter()
        .map(|r| {
            let VmValue::Int(i) = dict_get(r, "index") else {
                panic!()
            };
            *i
        })
        .collect();
    assert!(
        top_indices.contains(&1) && top_indices.contains(&3),
        "{top_indices:?}"
    );
}

#[test]
fn top_k_empty_corpus_is_empty() {
    let reg = registry_for(EmbedCapability::lexical());
    let out = invoke(
        &reg,
        "hostlib_embed_top_k",
        &[
            ("query", VmValue::string("anything")),
            ("corpus", VmValue::List(Arc::new(vec![]))),
            ("k", VmValue::Int(5)),
        ],
    );
    let VmValue::List(results) = dict_get(&out, "results") else {
        panic!()
    };
    assert!(results.is_empty());
}

#[test]
fn vector_dim_matches_info() {
    let reg = registry_for(EmbedCapability::lexical());
    let info = invoke(&reg, "hostlib_embed_info", &[]);
    let VmValue::Int(dim) = dict_get(&info, "dim") else {
        panic!()
    };
    let vec_out = invoke(
        &reg,
        "hostlib_embed_vector",
        &[("text", VmValue::string("symbol"))],
    );
    let VmValue::List(v) = dict_get(&vec_out, "vector") else {
        panic!()
    };
    assert_eq!(v.len() as i64, *dim);
}

#[test]
fn absent_static_asset_degrades_to_lexical() {
    // Sandbox/asset-absent contract: a missing model never errors, it just
    // selects the lexical floor. Builtins must still all work.
    let absent = std::env::temp_dir().join("embed-integration-absent-zzz");
    let _ = std::fs::remove_dir_all(&absent);
    let cap = EmbedCapability::resolve(Some(&absent), None, "potion");
    let reg = registry_for(cap);
    let info = invoke(&reg, "hostlib_embed_info", &[]);
    let VmValue::String(backend) = dict_get(&info, "backend") else {
        panic!()
    };
    assert_eq!(backend.as_str(), "lexical-hash");
}

#[test]
fn present_static_asset_is_used_end_to_end() {
    let dir = std::env::temp_dir().join("embed-integration-present-zzz");
    let _ = std::fs::create_dir_all(&dir);
    std::fs::write(
        dir.join("static-embeddings.json"),
        r#"{ "dim": 4, "vectors": {
            "rate": [1.0, 0.0, 0.0, 0.0],
            "limit": [0.0, 1.0, 0.0, 0.0],
            "throttle": [0.7071, 0.7071, 0.0, 0.0],
            "auth": [0.0, 0.0, 1.0, 0.0],
            "token": [0.0, 0.0, 0.0, 1.0]
        } }"#,
    )
    .unwrap();
    let cap = EmbedCapability::resolve(Some(&dir), None, "potion");
    let reg = registry_for(cap);

    let info = invoke(&reg, "hostlib_embed_info", &[]);
    let VmValue::String(backend) = dict_get(&info, "backend") else {
        panic!()
    };
    assert_eq!(backend.as_str(), "static-model2vec");

    // "rate limit" should be semantically close to "throttle" in this
    // hand-built static space — the whole point of static embeddings.
    let out = invoke(
        &reg,
        "hostlib_embed_similarity",
        &[
            ("a", VmValue::string("rate limit")),
            ("b", VmValue::string("throttle")),
        ],
    );
    assert!(as_f64(dict_get(&out, "similarity")) > 0.9);

    let _ = std::fs::remove_dir_all(&dir);
}
