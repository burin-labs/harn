//! Pure, dependency-light helpers for the neural backend.
//!
//! These carry no ONNX/tokenizer dependencies and are always compiled, so the
//! numeric and label-parsing logic is unit-tested in ordinary CI even though the
//! inference runtime itself (the `neural` feature) is not built there.

/// Numerically-stable softmax over `logits`. Returns an empty vector for empty
/// input. The result sums to 1 (within float error) for non-empty input.
pub(crate) fn softmax(logits: &[f32]) -> Vec<f32> {
    if logits.is_empty() {
        return Vec::new();
    }
    let max = logits.iter().copied().fold(f32::NEG_INFINITY, f32::max);
    let mut exps: Vec<f32> = logits.iter().map(|&x| (x - max).exp()).collect();
    let sum: f32 = exps.iter().sum();
    if sum > 0.0 {
        for value in &mut exps {
            *value /= sum;
        }
    }
    exps
}

/// Resolve which output index is the "injection / malicious" class from a
/// model's `config.json` `id2label` map (e.g. `{"0":"SAFE","1":"INJECTION"}`).
///
/// Picks the index whose label name names an attack; falls back to the last
/// (highest) index — the positive class of a binary safe/unsafe head — when no
/// label name matches. Clamped to `[0, num_labels)`.
pub(crate) fn resolve_injection_label(config_json: &serde_json::Value, num_labels: usize) -> usize {
    const ATTACK_MARKERS: &[&str] = &[
        "inject",
        "jailbreak",
        "malicious",
        "unsafe",
        "attack",
        "harmful",
        "label_1",
    ];
    let fallback = num_labels.saturating_sub(1);
    let Some(map) = config_json.get("id2label").and_then(|v| v.as_object()) else {
        return fallback;
    };
    let mut matched: Option<usize> = None;
    for (key, value) in map {
        let Some(label) = value.as_str() else {
            continue;
        };
        let label = label.to_ascii_lowercase();
        if ATTACK_MARKERS.iter().any(|marker| label.contains(marker)) {
            if let Ok(index) = key.parse::<usize>() {
                // Prefer the lowest matching index for determinism.
                matched = Some(matched.map_or(index, |existing| existing.min(index)));
            }
        }
    }
    matched
        .unwrap_or(fallback)
        .min(num_labels.saturating_sub(1))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn softmax_empty_is_empty() {
        assert!(softmax(&[]).is_empty());
    }

    #[test]
    fn softmax_sums_to_one_and_orders_by_logit() {
        let probs = softmax(&[2.0, 1.0, 0.1]);
        let sum: f32 = probs.iter().sum();
        assert!((sum - 1.0).abs() < 1e-5, "sum was {sum}");
        assert!(probs[0] > probs[1] && probs[1] > probs[2]);
    }

    #[test]
    fn softmax_is_stable_for_large_logits() {
        // Without the max-subtraction this overflows to NaN.
        let probs = softmax(&[1000.0, 1000.0]);
        assert!((probs[0] - 0.5).abs() < 1e-5);
        assert!(probs.iter().all(|p| p.is_finite()));
    }

    #[test]
    fn label_resolves_injection_by_name() {
        let cfg = json!({ "id2label": { "0": "SAFE", "1": "INJECTION" } });
        assert_eq!(resolve_injection_label(&cfg, 2), 1);
    }

    #[test]
    fn label_resolves_when_attack_is_index_zero() {
        let cfg = json!({ "id2label": { "0": "jailbreak", "1": "benign" } });
        assert_eq!(resolve_injection_label(&cfg, 2), 0);
    }

    #[test]
    fn label_falls_back_to_last_index_when_unknown() {
        let cfg = json!({ "id2label": { "0": "LABEL_0", "1": "LABEL_X" } });
        // No attack marker matches "label_x"; "label_1" isn't present, so fall
        // back to the highest index (the positive class).
        assert_eq!(resolve_injection_label(&cfg, 2), 1);
    }

    #[test]
    fn label_falls_back_without_id2label() {
        let cfg = json!({ "hidden_size": 768 });
        assert_eq!(resolve_injection_label(&cfg, 2), 1);
        assert_eq!(resolve_injection_label(&cfg, 1), 0);
    }

    #[test]
    fn label_is_clamped_into_range() {
        // A degenerate config claiming index 5 must still clamp under num_labels.
        let cfg = json!({ "id2label": { "5": "INJECTION" } });
        assert_eq!(resolve_injection_label(&cfg, 2), 1);
    }
}
