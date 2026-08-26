use std::collections::HashSet;
use std::sync::{Mutex, OnceLock};

pub(super) fn warn_unmatched_route_once(provider: &str, model: &str) {
    static WARNED: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    let first = WARNED
        .get_or_init(|| Mutex::new(HashSet::new()))
        .lock()
        .expect("unmatched capability warning cache poisoned")
        .insert(format!("{provider}/{model}"));
    if first {
        tracing::warn!(
            target: "harn::llm::capabilities",
            provider,
            model,
            "no capability rule or provider default matched; using conservative defaults"
        );
    }
}
