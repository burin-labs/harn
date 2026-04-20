use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use super::api::{LlmRequestPayload, LlmResult};
use super::cost::calculate_cost;

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct TriggerPredicateBudget {
    pub max_cost_usd: Option<f64>,
    pub tokens_max: Option<u64>,
    pub timeout_ms: Option<u64>,
}

impl TriggerPredicateBudget {
    pub fn timeout(&self) -> Option<Duration> {
        self.timeout_ms.map(Duration::from_millis)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct PredicateCacheEntry {
    pub request_hash: String,
    pub(crate) result: LlmResult,
}

#[derive(Clone, Debug, Default)]
pub struct PredicateEvaluationCapture {
    pub entries: Vec<PredicateCacheEntry>,
    pub total_tokens: u64,
    pub total_cost_usd: f64,
    pub cached: bool,
    pub budget_exceeded: bool,
}

#[derive(Clone, Debug, Default)]
struct PredicateEvaluationState {
    budget: TriggerPredicateBudget,
    replay_cache: HashMap<String, LlmResult>,
    entries: BTreeMap<String, LlmResult>,
    total_tokens: u64,
    total_cost_usd: f64,
    cached: bool,
    budget_exceeded: bool,
}

thread_local! {
    static ACTIVE_PREDICATE_EVALUATION: RefCell<Option<PredicateEvaluationState>> = const { RefCell::new(None) };
}

fn request_cache() -> &'static Mutex<HashMap<String, LlmResult>> {
    static CACHE: OnceLock<Mutex<HashMap<String, LlmResult>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

pub(crate) fn reset_trigger_predicate_state() {
    ACTIVE_PREDICATE_EVALUATION.with(|slot| {
        *slot.borrow_mut() = None;
    });
    if let Ok(mut cache) = request_cache().lock() {
        cache.clear();
    }
}

pub(crate) fn request_hash(request: &LlmRequestPayload) -> String {
    use std::hash::{Hash, Hasher};

    let canonical = serde_json::json!({
        "provider": request.provider,
        "model": request.model,
        "messages": request.messages,
        "system": request.system,
        "max_tokens": request.max_tokens,
        "temperature": request.temperature,
        "top_p": request.top_p,
        "top_k": request.top_k,
        "stop": request.stop,
        "seed": request.seed,
        "frequency_penalty": request.frequency_penalty,
        "presence_penalty": request.presence_penalty,
        "response_format": request.response_format,
        "json_schema": request.json_schema,
        "thinking": request.thinking,
        "native_tools": request.native_tools,
        "tool_choice": request.tool_choice,
        "cache": request.cache,
        "timeout": request.timeout,
        "stream": request.stream,
        "provider_overrides": request.provider_overrides,
        "prefill": request.prefill,
    });
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    serde_json::to_string(&canonical)
        .unwrap_or_default()
        .hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

pub(crate) struct PredicateEvaluationGuard;

impl PredicateEvaluationGuard {
    pub fn finish(self) -> PredicateEvaluationCapture {
        finish_predicate_evaluation()
    }
}

impl Drop for PredicateEvaluationGuard {
    fn drop(&mut self) {
        ACTIVE_PREDICATE_EVALUATION.with(|slot| {
            *slot.borrow_mut() = None;
        });
    }
}

pub(crate) fn start_predicate_evaluation(
    budget: TriggerPredicateBudget,
    replay_entries: Vec<PredicateCacheEntry>,
) -> PredicateEvaluationGuard {
    ACTIVE_PREDICATE_EVALUATION.with(|slot| {
        *slot.borrow_mut() = Some(PredicateEvaluationState {
            budget,
            replay_cache: replay_entries
                .into_iter()
                .map(|entry| (entry.request_hash, entry.result))
                .collect(),
            ..Default::default()
        });
    });
    PredicateEvaluationGuard
}

fn finish_predicate_evaluation() -> PredicateEvaluationCapture {
    ACTIVE_PREDICATE_EVALUATION.with(|slot| {
        let Some(state) = slot.borrow_mut().take() else {
            return PredicateEvaluationCapture::default();
        };
        PredicateEvaluationCapture {
            entries: state
                .entries
                .into_iter()
                .map(|(request_hash, result)| PredicateCacheEntry {
                    request_hash,
                    result,
                })
                .collect(),
            total_tokens: state.total_tokens,
            total_cost_usd: state.total_cost_usd,
            cached: state.cached,
            budget_exceeded: state.budget_exceeded,
        }
    })
}

pub(crate) fn lookup_cached_result(request: &LlmRequestPayload) -> Option<LlmResult> {
    ACTIVE_PREDICATE_EVALUATION.with(|slot| {
        let mut borrowed = slot.borrow_mut();
        let state = borrowed.as_mut()?;
        if state.budget_exceeded {
            return None;
        }
        let hash = request_hash(request);
        let cached = state.replay_cache.get(&hash).cloned().or_else(|| {
            request_cache()
                .lock()
                .ok()
                .and_then(|cache| cache.get(&hash).cloned())
        });
        if let Some(result) = cached.clone() {
            state.cached = true;
            state.entries.insert(hash, result.clone());
            return Some(result);
        }
        None
    })
}

pub(crate) fn note_result(request: &LlmRequestPayload, result: &LlmResult) {
    ACTIVE_PREDICATE_EVALUATION.with(|slot| {
        let mut borrowed = slot.borrow_mut();
        let Some(state) = borrowed.as_mut() else {
            return;
        };
        let hash = request_hash(request);
        state.entries.insert(hash.clone(), result.clone());
        if let Ok(mut cache) = request_cache().lock() {
            cache.insert(hash, result.clone());
        }
        let call_tokens = result
            .input_tokens
            .saturating_add(result.output_tokens)
            .max(0) as u64;
        state.total_tokens = state.total_tokens.saturating_add(call_tokens);
        state.total_cost_usd +=
            calculate_cost(&result.model, result.input_tokens, result.output_tokens);
        if state
            .budget
            .tokens_max
            .is_some_and(|limit| state.total_tokens > limit)
        {
            state.budget_exceeded = true;
        }
        if state
            .budget
            .max_cost_usd
            .is_some_and(|limit| state.total_cost_usd > limit)
        {
            state.budget_exceeded = true;
        }
    });
}
