//! `assemble_context` stdlib builtin: the Harn-facing wrapper around the
//! [`crate::orchestration::assemble_context`] core.
//!
//! The builtin is registered on the agent tier because the pluggable
//! ranker is invoked as a Harn closure, which requires an async-builtin
//! VM context (`clone_async_builtin_child_vm`).

use std::collections::BTreeMap;
use std::rc::Rc;

use crate::orchestration::{
    assemble_context as core_assemble, build_candidate_chunks, ArtifactRecord, AssembleDedup,
    AssembleOptions, AssembleStrategy, AssembledChunk, AssembledContext,
};
use crate::value::{VmError, VmValue};
use crate::vm::Vm;

use super::agents::parse_artifact_list;

pub(crate) fn register_assemble_context_builtin(vm: &mut Vm) {
    vm.register_async_builtin("assemble_context", |args| async move {
        assemble_context_impl(args).await
    });
}

async fn assemble_context_impl(args: Vec<VmValue>) -> Result<VmValue, VmError> {
    let options_value = args.first().cloned().unwrap_or(VmValue::Nil);
    let dict = options_value.as_dict().ok_or_else(|| {
        VmError::Runtime(
            "assemble_context: first argument must be an options dict with `artifacts`".to_string(),
        )
    })?;
    let artifacts = parse_artifact_list(dict.get("artifacts"))?;
    let options = parse_assemble_options(dict)?;

    let ranker = dict.get("ranker_callback").cloned();

    let custom_scores = if matches!(&ranker, Some(VmValue::Closure(_)))
        && options.strategy == AssembleStrategy::Relevance
    {
        let mut candidate_dropped = Vec::new();
        let candidates = build_candidate_chunks(&artifacts, &options, &mut candidate_dropped);
        let query_vm =
            VmValue::String(Rc::from(options.query.clone().unwrap_or_default().as_str()));
        let chunks_vm = VmValue::List(Rc::new(candidates.iter().map(chunk_to_ranker_vm).collect()));
        let scores = invoke_ranker_callback(
            ranker.as_ref().unwrap(),
            &query_vm,
            &chunks_vm,
            candidates.len(),
        )
        .await?;
        Some(scores)
    } else {
        None
    };

    let assembled = core_assemble(&artifacts, &options, custom_scores.as_deref());
    Ok(assembled_to_vm(&assembled))
}

fn parse_assemble_options(dict: &BTreeMap<String, VmValue>) -> Result<AssembleOptions, VmError> {
    let defaults = AssembleOptions::default();
    let mut options = defaults.clone();
    if let Some(value) = dict.get("budget_tokens").and_then(VmValue::as_int) {
        if value < 0 {
            return Err(VmError::Runtime(
                "assemble_context: budget_tokens must be >= 0".to_string(),
            ));
        }
        options.budget_tokens = value as usize;
    }
    if let Some(value) = dict.get("microcompact_threshold").and_then(VmValue::as_int) {
        if value < 0 {
            return Err(VmError::Runtime(
                "assemble_context: microcompact_threshold must be >= 0".to_string(),
            ));
        }
        options.microcompact_threshold = value as usize;
    }
    if let Some(VmValue::String(text)) = dict.get("dedup") {
        options.dedup = AssembleDedup::parse(text.as_ref()).map_err(VmError::Runtime)?;
    }
    if let Some(VmValue::String(text)) = dict.get("strategy") {
        options.strategy = AssembleStrategy::parse(text.as_ref()).map_err(VmError::Runtime)?;
    }
    if let Some(VmValue::String(text)) = dict.get("query") {
        if !text.trim().is_empty() {
            options.query = Some(text.to_string());
        }
    }
    if let Some(value) = dict.get("semantic_overlap") {
        if let Some(f) = value_as_float(value) {
            if !(0.0..=1.0).contains(&f) {
                return Err(VmError::Runtime(
                    "assemble_context: semantic_overlap must be in [0.0, 1.0]".to_string(),
                ));
            }
            options.semantic_overlap = f;
        }
    }
    Ok(options)
}

fn value_as_float(value: &VmValue) -> Option<f64> {
    match value {
        VmValue::Float(number) => Some(*number),
        VmValue::Int(number) => Some(*number as f64),
        _ => None,
    }
}

/// Compact VM representation of a chunk for the ranker callback.
fn chunk_to_ranker_vm(chunk: &AssembledChunk) -> VmValue {
    let mut map = BTreeMap::new();
    map.insert(
        "id".to_string(),
        VmValue::String(Rc::from(chunk.id.as_str())),
    );
    map.insert(
        "artifact_id".to_string(),
        VmValue::String(Rc::from(chunk.artifact_id.as_str())),
    );
    map.insert(
        "artifact_kind".to_string(),
        VmValue::String(Rc::from(chunk.artifact_kind.as_str())),
    );
    if let Some(title) = chunk.title.as_ref() {
        map.insert(
            "title".to_string(),
            VmValue::String(Rc::from(title.as_str())),
        );
    } else {
        map.insert("title".to_string(), VmValue::Nil);
    }
    if let Some(source) = chunk.source.as_ref() {
        map.insert(
            "source".to_string(),
            VmValue::String(Rc::from(source.as_str())),
        );
    } else {
        map.insert("source".to_string(), VmValue::Nil);
    }
    map.insert(
        "text".to_string(),
        VmValue::String(Rc::from(chunk.text.as_str())),
    );
    map.insert(
        "estimated_tokens".to_string(),
        VmValue::Int(chunk.estimated_tokens as i64),
    );
    map.insert(
        "chunk_index".to_string(),
        VmValue::Int(chunk.chunk_index as i64),
    );
    map.insert(
        "chunk_count".to_string(),
        VmValue::Int(chunk.chunk_count as i64),
    );
    VmValue::Dict(Rc::new(map))
}

async fn invoke_ranker_callback(
    callback: &VmValue,
    query: &VmValue,
    chunks: &VmValue,
    expected_len: usize,
) -> Result<Vec<f64>, VmError> {
    let VmValue::Closure(closure) = callback.clone() else {
        return Err(VmError::Runtime(
            "assemble_context: ranker_callback must be a closure".to_string(),
        ));
    };
    let mut vm = crate::vm::clone_async_builtin_child_vm().ok_or_else(|| {
        VmError::Runtime(
            "assemble_context: ranker_callback requires an async builtin VM context".to_string(),
        )
    })?;
    let result = vm
        .call_closure_pub(&closure, &[query.clone(), chunks.clone()])
        .await?;
    let list = match result {
        VmValue::List(items) => items,
        _ => {
            return Err(VmError::Runtime(
                "assemble_context: ranker_callback must return a list of numbers".to_string(),
            ));
        }
    };
    let mut scores = Vec::with_capacity(expected_len);
    for value in list.iter() {
        let score = match value {
            VmValue::Float(n) => *n,
            VmValue::Int(n) => *n as f64,
            VmValue::Nil => 0.0,
            other => {
                return Err(VmError::Runtime(format!(
                    "assemble_context: ranker_callback score must be a number (got {})",
                    other.type_name()
                )));
            }
        };
        scores.push(score);
    }
    // Pad or truncate so downstream indexing can zip without panicking.
    scores.resize(expected_len, 0.0);
    Ok(scores)
}

fn assembled_to_vm(assembled: &AssembledContext) -> VmValue {
    let chunks: Vec<VmValue> = assembled
        .chunks
        .iter()
        .map(|chunk| {
            let mut map = BTreeMap::new();
            map.insert(
                "id".to_string(),
                VmValue::String(Rc::from(chunk.id.as_str())),
            );
            map.insert(
                "artifact_id".to_string(),
                VmValue::String(Rc::from(chunk.artifact_id.as_str())),
            );
            map.insert(
                "artifact_kind".to_string(),
                VmValue::String(Rc::from(chunk.artifact_kind.as_str())),
            );
            map.insert(
                "title".to_string(),
                chunk
                    .title
                    .as_ref()
                    .map(|title| VmValue::String(Rc::from(title.as_str())))
                    .unwrap_or(VmValue::Nil),
            );
            map.insert(
                "source".to_string(),
                chunk
                    .source
                    .as_ref()
                    .map(|source| VmValue::String(Rc::from(source.as_str())))
                    .unwrap_or(VmValue::Nil),
            );
            map.insert(
                "text".to_string(),
                VmValue::String(Rc::from(chunk.text.as_str())),
            );
            map.insert(
                "estimated_tokens".to_string(),
                VmValue::Int(chunk.estimated_tokens as i64),
            );
            map.insert(
                "chunk_index".to_string(),
                VmValue::Int(chunk.chunk_index as i64),
            );
            map.insert(
                "chunk_count".to_string(),
                VmValue::Int(chunk.chunk_count as i64),
            );
            map.insert("score".to_string(), VmValue::Float(chunk.score));
            VmValue::Dict(Rc::new(map))
        })
        .collect();

    let included: Vec<VmValue> = assembled
        .included
        .iter()
        .map(|summary| {
            let mut map = BTreeMap::new();
            map.insert(
                "artifact_id".to_string(),
                VmValue::String(Rc::from(summary.artifact_id.as_str())),
            );
            map.insert(
                "artifact_kind".to_string(),
                VmValue::String(Rc::from(summary.artifact_kind.as_str())),
            );
            map.insert(
                "chunks_included".to_string(),
                VmValue::Int(summary.chunks_included as i64),
            );
            map.insert(
                "chunks_total".to_string(),
                VmValue::Int(summary.chunks_total as i64),
            );
            map.insert(
                "tokens_included".to_string(),
                VmValue::Int(summary.tokens_included as i64),
            );
            VmValue::Dict(Rc::new(map))
        })
        .collect();

    let dropped: Vec<VmValue> = assembled
        .dropped
        .iter()
        .map(|exclusion| {
            let mut map = BTreeMap::new();
            map.insert(
                "artifact_id".to_string(),
                VmValue::String(Rc::from(exclusion.artifact_id.as_str())),
            );
            map.insert(
                "chunk_id".to_string(),
                exclusion
                    .chunk_id
                    .as_ref()
                    .map(|id| VmValue::String(Rc::from(id.as_str())))
                    .unwrap_or(VmValue::Nil),
            );
            map.insert(
                "reason".to_string(),
                VmValue::String(Rc::from(exclusion.reason)),
            );
            map.insert(
                "detail".to_string(),
                exclusion
                    .detail
                    .as_ref()
                    .map(|text| VmValue::String(Rc::from(text.as_str())))
                    .unwrap_or(VmValue::Nil),
            );
            VmValue::Dict(Rc::new(map))
        })
        .collect();

    let reasons: Vec<VmValue> = assembled
        .reasons
        .iter()
        .map(|reason| {
            let mut map = BTreeMap::new();
            map.insert(
                "chunk_id".to_string(),
                VmValue::String(Rc::from(reason.chunk_id.as_str())),
            );
            map.insert(
                "artifact_id".to_string(),
                VmValue::String(Rc::from(reason.artifact_id.as_str())),
            );
            map.insert(
                "strategy".to_string(),
                VmValue::String(Rc::from(reason.strategy)),
            );
            map.insert("score".to_string(), VmValue::Float(reason.score));
            map.insert("included".to_string(), VmValue::Bool(reason.included));
            map.insert(
                "reason".to_string(),
                VmValue::String(Rc::from(reason.reason)),
            );
            VmValue::Dict(Rc::new(map))
        })
        .collect();

    let mut map = BTreeMap::new();
    map.insert("chunks".to_string(), VmValue::List(Rc::new(chunks)));
    map.insert("included".to_string(), VmValue::List(Rc::new(included)));
    map.insert("dropped".to_string(), VmValue::List(Rc::new(dropped)));
    map.insert("reasons".to_string(), VmValue::List(Rc::new(reasons)));
    map.insert(
        "total_tokens".to_string(),
        VmValue::Int(assembled.total_tokens as i64),
    );
    map.insert(
        "budget_tokens".to_string(),
        VmValue::Int(assembled.budget_tokens as i64),
    );
    map.insert(
        "strategy".to_string(),
        VmValue::String(Rc::from(assembled.strategy.as_str())),
    );
    map.insert(
        "dedup".to_string(),
        VmValue::String(Rc::from(assembled.dedup.as_str())),
    );
    VmValue::Dict(Rc::new(map))
}

/// Convenience entry point used by the agent_loop integration hook:
/// parse the same options dict shape but without requiring an artifacts
/// array (the caller supplies them separately).
pub async fn assemble_from_options(
    artifacts: &[ArtifactRecord],
    options_value: &VmValue,
) -> Result<AssembledContext, VmError> {
    let dict = options_value.as_dict().ok_or_else(|| {
        VmError::Runtime("assemble_context (hook): options must be a dict".to_string())
    })?;
    let options = parse_assemble_options(dict)?;
    let ranker = dict.get("ranker_callback").cloned();
    let custom_scores = if matches!(&ranker, Some(VmValue::Closure(_)))
        && options.strategy == AssembleStrategy::Relevance
    {
        let mut candidate_dropped = Vec::new();
        let candidates = build_candidate_chunks(artifacts, &options, &mut candidate_dropped);
        let query_vm =
            VmValue::String(Rc::from(options.query.clone().unwrap_or_default().as_str()));
        let chunks_vm = VmValue::List(Rc::new(candidates.iter().map(chunk_to_ranker_vm).collect()));
        Some(
            invoke_ranker_callback(
                ranker.as_ref().unwrap(),
                &query_vm,
                &chunks_vm,
                candidates.len(),
            )
            .await?,
        )
    } else {
        None
    };
    Ok(core_assemble(artifacts, &options, custom_scores.as_deref()))
}
