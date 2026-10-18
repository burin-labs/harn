use super::ToolApprovalPolicy;

const EVIDENCE_MAX_CHARS: usize = 240;

pub(crate) fn first_candidate(
    policy: &ToolApprovalPolicy,
    candidates: &[String],
) -> Option<String> {
    let custom = &policy.sensitive_path_patterns;
    candidates
        .iter()
        .find(|candidate| {
            if custom.is_empty() {
                crate::known_paths::is_credential_path(candidate)
            } else {
                is_candidate(candidate, custom.iter().map(String::as_str))
            }
        })
        .cloned()
}

/// Keep the basename that justified a denial visible while bounding host/model
/// evidence. Prefix-only evidence is both noisy and unable to explain a match
/// at the end of a long path.
pub(super) fn bounded_evidence(path: &str) -> String {
    crate::text::truncate_start(path, EVIDENCE_MAX_CHARS)
}

fn is_candidate<'a>(candidate: &str, patterns: impl IntoIterator<Item = &'a str>) -> bool {
    let normalized = candidate.replace('\\', "/").to_ascii_lowercase();
    let basename = normalized.rsplit('/').next().unwrap_or(normalized.as_str());
    patterns.into_iter().any(|pattern| {
        let pattern = pattern.to_ascii_lowercase();
        harn_glob::match_path(&pattern, &normalized)
            || harn_glob::match_path(&pattern, basename)
            // `sensitive_path_patterns` historically accepted the full flat
            // glob grammar. Keep character classes/alternates and the old
            // slash-crossing `*` behavior as a compatibility superset while
            // adding the path-aware matcher above for new policies.
            || harn_glob::match_name(&pattern, &normalized)
            || harn_glob::match_name(&pattern, basename)
    })
}
