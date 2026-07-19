//! The unknown-key gate: every caller-supplied `llm_call` options dict is
//! validated against the canonical option registry
//! (`harn_builtin_meta::llm_options`) before any parsing happens.
//!
//! Three outcomes per key:
//! * `_`-prefixed — host/agent-loop plumbing channel, accepted verbatim;
//! * registered — parsed by the extractor cascade (or knowingly consumed by
//!   the stdlib wrapper plane);
//! * anything else — a hard typed error: removed keys carry their recorded
//!   replacement, unknown keys a nearest-match suggestion. Nothing is ever
//!   silently dropped or silently ignored.

use super::*;
use harn_builtin_meta::llm_options::{
    is_llm_call_option, removed_llm_option, LLM_CALL_OPTION_FIELDS,
};

/// Validate every key of a caller-supplied options dict. Runs on the RAW
/// dict, before context merges (`merge_context_options`) or default injection
/// can add host-owned keys.
pub(crate) fn validate_llm_option_keys(options: &crate::value::DictMap) -> Result<(), VmError> {
    for (key, _) in options.iter() {
        let key: &str = key.as_ref();
        if key.starts_with('_') {
            continue;
        }
        if let Some(entry) = removed_llm_option(key) {
            return Err(VmError::Thrown(VmValue::String(arcstr::ArcStr::from(
                format!("llm_call: option `{key}` was removed — {}", entry.fix),
            ))));
        }
        if !is_llm_call_option(key) {
            return Err(VmError::Thrown(VmValue::String(arcstr::ArcStr::from(
                unknown_key_message(key),
            ))));
        }
    }
    Ok(())
}

/// Select canonical LLM options from a broader wrapper/control dict. Removed
/// spellings still fail; unrelated wrapper keys are intentionally ignored.
pub(crate) fn project_llm_options(
    options: &crate::value::DictMap,
) -> Result<crate::value::DictMap, VmError> {
    let mut projected = crate::value::DictMap::new();
    for (key, value) in options.iter() {
        let name: &str = key.as_ref();
        if let Some(entry) = removed_llm_option(name) {
            return Err(VmError::Thrown(VmValue::String(arcstr::ArcStr::from(
                format!("llm_call: option `{name}` was removed — {}", entry.fix),
            ))));
        }
        if name.starts_with('_') || is_llm_call_option(name) {
            projected.insert(key.clone(), value.clone());
        }
    }
    Ok(projected)
}

fn unknown_key_message(key: &str) -> String {
    match nearest_registry_key(key) {
        Some(suggestion) => format!(
            "llm_call: unknown option `{key}` — did you mean `{suggestion}`? \
             See docs/llm/llm_call.md for the option reference."
        ),
        None => format!(
            "llm_call: unknown option `{key}`. \
             See docs/llm/llm_call.md for the option reference."
        ),
    }
}

/// Nearest registered key by edit distance, when the distance is small enough
/// to plausibly be a typo (≤ 2, or ≤ 3 for keys longer than 8 chars).
fn nearest_registry_key(key: &str) -> Option<&'static str> {
    let mut best: Option<(usize, &'static str)> = None;
    for field in LLM_CALL_OPTION_FIELDS {
        let d = edit_distance(key, field.name);
        if best.map(|(bd, _)| d < bd).unwrap_or(true) {
            best = Some((d, field.name));
        }
    }
    let (distance, name) = best?;
    let budget = if key.len() > 8 { 3 } else { 2 };
    (distance <= budget).then_some(name)
}

/// Classic two-row Levenshtein — the registry is ~70 short keys, so this runs
/// in microseconds and only on the error path.
fn edit_distance(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut cur = vec![0usize; b.len() + 1];
    for (i, ca) in a.iter().enumerate() {
        cur[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let sub = prev[j] + usize::from(ca != cb);
            cur[j + 1] = sub.min(prev[j + 1] + 1).min(cur[j] + 1);
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[b.len()]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::value::{DictMap, VmDictExt, VmValue};

    fn message(err: VmError) -> String {
        match err {
            VmError::Thrown(VmValue::String(s)) => s.to_string(),
            other => format!("{other:?}"),
        }
    }

    #[test]
    fn accepts_registered_and_internal_keys() {
        let mut dict = DictMap::new();
        dict.put_str("model", "m");
        dict.put_str("provider", "mock");
        dict.put("_iteration", VmValue::Int(3));
        dict.put("timeout_ms", VmValue::Int(1500));
        assert!(validate_llm_option_keys(&dict).is_ok());
    }

    #[test]
    fn unknown_key_errors_with_suggestion() {
        let mut dict = DictMap::new();
        dict.put("temperatur", VmValue::Float(0.2));
        let msg = message(validate_llm_option_keys(&dict).unwrap_err());
        assert!(msg.contains("unknown option `temperatur`"), "{msg}");
        assert!(msg.contains("did you mean `temperature`"), "{msg}");
    }

    #[test]
    fn removed_key_errors_with_fix() {
        let mut dict = DictMap::new();
        dict.put("json_schema", VmValue::Nil);
        let msg = message(validate_llm_option_keys(&dict).unwrap_err());
        assert!(msg.contains("`json_schema` was removed"), "{msg}");
        assert!(msg.contains("output"), "{msg}");
    }

    #[test]
    fn every_removed_registry_key_is_rejected_with_its_fix() {
        for entry in harn_builtin_meta::llm_options::LLM_REMOVED_OPTIONS {
            let mut dict = DictMap::new();
            dict.put(entry.key, VmValue::Nil);
            let msg = message(validate_llm_option_keys(&dict).unwrap_err());
            assert!(msg.contains(entry.key), "missing key in diagnostic: {msg}");
            assert!(
                msg.contains(entry.fix),
                "missing fix for {}: {msg}",
                entry.key
            );
        }
    }

    #[test]
    fn projection_preserves_canonical_options_and_drops_wrapper_controls() {
        let mut dict = DictMap::new();
        dict.put_str("model", "m");
        dict.put_str("provider", "mock");
        dict.put("keep_last", VmValue::Int(4));

        let projected = project_llm_options(&dict).unwrap();
        assert_eq!(
            projected.get("model").map(VmValue::display).as_deref(),
            Some("m")
        );
        assert_eq!(
            projected.get("provider").map(VmValue::display).as_deref(),
            Some("mock")
        );
        assert!(!projected.contains_key("keep_last"));
    }

    #[test]
    fn projection_rejects_removed_options() {
        let mut dict = DictMap::new();
        dict.put("output_schema", VmValue::Nil);
        let msg = message(project_llm_options(&dict).unwrap_err());
        assert!(msg.contains("`output_schema` was removed"), "{msg}");
        assert!(msg.contains("output"), "{msg}");
    }

    #[test]
    fn bare_provider_key_names_the_namespaced_rewrite() {
        let mut dict = DictMap::new();
        dict.put("openai", VmValue::dict(DictMap::new()));
        let msg = message(validate_llm_option_keys(&dict).unwrap_err());
        assert!(msg.contains("provider_options: {openai:"), "{msg}");
    }
}
