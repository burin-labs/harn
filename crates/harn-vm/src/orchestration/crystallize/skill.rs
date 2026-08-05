//! Skill induction adapter for crystallized workflow candidates.
//!
//! The miner still produces workflow candidates. This module projects
//! those candidates into compact `SKILL.md` artifacts and admits the
//! skill only when the existing shadow/replay report covers the source
//! trajectory and at least one held-out sibling trace.

use std::collections::{BTreeMap, BTreeSet};

use super::types::{
    CrystallizationReport, CrystallizationTrace, SegmentKind, ShadowTraceResult,
    SkillCandidateArtifact, SkillCandidateEvidenceRef, SkillCandidateEvidenceRole,
    SkillInductionGateReceipt, SkillInductionReplayGate, WorkflowCandidate, SKILL_CANDIDATE_SCHEMA,
    SKILL_CANDIDATE_SCHEMA_VERSION, SKILL_GATE_RECEIPT_SCHEMA,
};
use super::util::{hash_bytes, sanitize_identifier, sorted_strings};

pub fn refresh_skill_candidates(
    report: &mut CrystallizationReport,
    traces: &[CrystallizationTrace],
) {
    let mut accepted = Vec::new();
    let mut rejected = Vec::new();
    for candidate in report
        .candidates
        .iter()
        .chain(report.rejected_candidates.iter())
    {
        let skill = induce_skill_candidate(candidate, traces);
        if skill.is_safe_to_propose() {
            accepted.push(skill);
        } else {
            rejected.push(skill);
        }
    }
    report.skill_candidates = accepted;
    report.rejected_skill_candidates = rejected;
}

pub fn induce_skill_candidate(
    candidate: &WorkflowCandidate,
    traces: &[CrystallizationTrace],
) -> SkillCandidateArtifact {
    let evidence_refs = evidence_refs(candidate, traces);
    let source_trace_hashes = sorted_strings(
        evidence_refs
            .iter()
            .filter(|evidence| evidence.role == SkillCandidateEvidenceRole::Source)
            .map(|evidence| evidence.source_hash.clone())
            .filter(|hash| !hash.is_empty()),
    );
    let name = skill_name(candidate);
    let description = description_for(candidate);
    let short = short_for(candidate);
    let when_to_use = when_to_use_for(candidate, &evidence_refs);
    let allowed_tools = allowed_tools_for(candidate);
    let paths = paths_for(candidate);
    let mut warnings = candidate.warnings.clone();
    if candidate
        .steps
        .iter()
        .any(|step| step.segment == SegmentKind::Fuzzy)
    {
        warnings.push(
            "candidate contains fuzzy steps; the skill must preserve review boundaries".to_string(),
        );
    }
    let mut rejection_reasons = candidate.rejection_reasons.clone();
    let replay_gate = replay_gate_for(candidate, &evidence_refs, &mut rejection_reasons);
    let mut skill = SkillCandidateArtifact {
        schema: SKILL_CANDIDATE_SCHEMA.to_string(),
        schema_version: SKILL_CANDIDATE_SCHEMA_VERSION,
        id: skill_candidate_id(candidate),
        workflow_candidate_id: candidate.id.clone(),
        name,
        short,
        description,
        when_to_use,
        allowed_tools,
        paths,
        source_trace_hashes,
        evidence_refs,
        replay_gate,
        skill_markdown: String::new(),
        warnings,
        rejection_reasons,
    };
    skill.skill_markdown = render_skill_markdown(candidate, &skill);
    skill
}

fn skill_candidate_id(candidate: &WorkflowCandidate) -> String {
    format!(
        "skill_{}",
        hash_bytes(format!("{}:{}", candidate.id, candidate.name).as_bytes())
            .trim_start_matches("sha256:")
            .chars()
            .take(16)
            .collect::<String>()
    )
}

fn skill_name(candidate: &WorkflowCandidate) -> String {
    let base = sanitize_identifier(&candidate.name.replace('-', "_"));
    if base.is_empty() {
        format!("induced_{}", candidate.id.replace('-', "_"))
    } else {
        format!("{base}_skill")
    }
}

fn description_for(candidate: &WorkflowCandidate) -> String {
    let sequence = candidate
        .steps
        .iter()
        .map(|step| step.name.as_str())
        .collect::<Vec<_>>()
        .join(" -> ");
    format!(
        "Replay-gated skill induced from the {} workflow candidate; use it to guide sibling tasks that follow {}.",
        candidate.name, sequence
    )
}

fn short_for(candidate: &WorkflowCandidate) -> String {
    let verbs = candidate
        .steps
        .iter()
        .filter(|step| step.kind == "tool_call" || step.kind == "file_mutation")
        .map(|step| step.name.as_str())
        .take(3)
        .collect::<Vec<_>>();
    if verbs.is_empty() {
        format!(
            "Use for sibling tasks matching the replay-gated {} workflow.",
            candidate.name
        )
    } else {
        format!(
            "Use for sibling tasks that need {} in the replay-gated {} workflow.",
            verbs.join(", "),
            candidate.name
        )
    }
}

fn when_to_use_for(
    candidate: &WorkflowCandidate,
    evidence_refs: &[SkillCandidateEvidenceRef],
) -> String {
    let mut parts = Vec::new();
    parts.push(format!(
        "The task asks for the same outcome as workflow `{}`.",
        candidate.name
    ));
    let step_names = candidate
        .steps
        .iter()
        .map(|step| step.name.as_str())
        .collect::<Vec<_>>();
    if !step_names.is_empty() {
        parts.push(format!("Expected step pattern: {}.", step_names.join(", ")));
    }
    let heldout_count = evidence_refs
        .iter()
        .filter(|evidence| evidence.role == SkillCandidateEvidenceRole::HeldOut)
        .count();
    if heldout_count > 0 {
        parts.push(format!(
            "Activation was validated on {heldout_count} held-out sibling trace(s)."
        ));
    }
    parts.join(" ")
}

fn allowed_tools_for(candidate: &WorkflowCandidate) -> Vec<String> {
    let tools = candidate
        .steps
        .iter()
        .filter(|step| step.kind == "tool_call")
        .map(|step| step.name.clone())
        .collect::<Vec<_>>();
    if tools.is_empty() {
        Vec::new()
    } else {
        sorted_strings(tools.into_iter())
    }
}

fn paths_for(candidate: &WorkflowCandidate) -> Vec<String> {
    let mut paths = BTreeSet::new();
    for step in &candidate.steps {
        for value in step.constants.values() {
            if let Some(path) = value.as_str().filter(|value| looks_like_path(value)) {
                paths.insert(path.to_string());
            }
        }
        for effect in &step.side_effects {
            if looks_like_path(&effect.target) {
                paths.insert(effect.target.clone());
            }
        }
    }
    paths.into_iter().collect()
}

fn looks_like_path(value: &str) -> bool {
    let trimmed = value.trim();
    !trimmed.is_empty()
        && !trimmed.contains("://")
        && !trimmed.starts_with('/')
        && !trimmed.starts_with("~/")
        && !trimmed
            .get(1..3)
            .is_some_and(|prefix| prefix.starts_with(":\\") || prefix.starts_with(":/"))
        && (trimmed.contains('/') || trimmed.contains('.') || trimmed.starts_with('*'))
}

fn evidence_refs(
    candidate: &WorkflowCandidate,
    traces: &[CrystallizationTrace],
) -> Vec<SkillCandidateEvidenceRef> {
    let source_ids = candidate
        .examples
        .iter()
        .map(|example| example.trace_id.as_str())
        .collect::<BTreeSet<_>>();
    let action_ids_by_trace = candidate
        .examples
        .iter()
        .map(|example| (example.trace_id.as_str(), example.action_ids.clone()))
        .collect::<BTreeMap<_, _>>();
    candidate
        .shadow
        .traces
        .iter()
        .map(|shadow| {
            let trace = traces.iter().find(|trace| trace.id == shadow.trace_id);
            SkillCandidateEvidenceRef {
                trace_id: shadow.trace_id.clone(),
                source_hash: trace
                    .and_then(|trace| trace.source_hash.clone())
                    .unwrap_or_else(|| shadow.source_hash.clone()),
                source_url: trace.and_then(|trace| trace.source.clone()),
                action_ids: action_ids_by_trace
                    .get(shadow.trace_id.as_str())
                    .cloned()
                    .or_else(|| {
                        trace.map(|trace| trace.actions.iter().map(|a| a.id.clone()).collect())
                    })
                    .unwrap_or_default(),
                role: if source_ids.contains(shadow.trace_id.as_str()) {
                    SkillCandidateEvidenceRole::Source
                } else {
                    SkillCandidateEvidenceRole::HeldOut
                },
            }
        })
        .collect()
}

fn replay_gate_for(
    candidate: &WorkflowCandidate,
    evidence_refs: &[SkillCandidateEvidenceRef],
    rejection_reasons: &mut Vec<String>,
) -> SkillInductionReplayGate {
    let source_ids = evidence_refs
        .iter()
        .filter(|evidence| evidence.role == SkillCandidateEvidenceRole::Source)
        .map(|evidence| evidence.trace_id.as_str())
        .collect::<BTreeSet<_>>();
    let heldout_ids = evidence_refs
        .iter()
        .filter(|evidence| evidence.role == SkillCandidateEvidenceRole::HeldOut)
        .map(|evidence| evidence.trace_id.as_str())
        .collect::<BTreeSet<_>>();
    let mut failures = candidate.shadow.failures.clone();
    let original_replay_pass = trace_group_passes(&candidate.shadow.traces, &source_ids);
    let heldout_replay_pass =
        !heldout_ids.is_empty() && trace_group_passes(&candidate.shadow.traces, &heldout_ids);

    if !original_replay_pass {
        failures.push("source trajectory replay/shadow gate failed".to_string());
    }
    if heldout_ids.is_empty() {
        failures.push(
            "skill induction requires at least one held-out sibling trace before acceptance"
                .to_string(),
        );
    } else if !heldout_replay_pass {
        failures.push("held-out sibling replay/shadow gate failed".to_string());
    }
    for failure in &failures {
        if !rejection_reasons.contains(failure) {
            rejection_reasons.push(failure.clone());
        }
    }

    let accepted = original_replay_pass && heldout_replay_pass && rejection_reasons.is_empty();
    let receipt = SkillInductionGateReceipt {
        type_name: SKILL_GATE_RECEIPT_SCHEMA.to_string(),
        schema_version: SKILL_CANDIDATE_SCHEMA_VERSION,
        skill_candidate_id: skill_candidate_id(candidate),
        workflow_candidate_id: candidate.id.clone(),
        accepted,
        decision: if accepted { "accepted" } else { "rejected" }.to_string(),
        original_trace_count: source_ids.len(),
        heldout_trace_count: heldout_ids.len(),
        compared_trace_count: candidate.shadow.compared_traces,
        failures: sorted_strings(failures.clone().into_iter()),
        replay_trace_ids: source_ids.iter().map(|id| (*id).to_string()).collect(),
        heldout_trace_ids: heldout_ids.iter().map(|id| (*id).to_string()).collect(),
    };

    SkillInductionReplayGate {
        original_replay_pass,
        heldout_replay_pass,
        original_trace_count: source_ids.len(),
        heldout_trace_count: heldout_ids.len(),
        compared_trace_count: candidate.shadow.compared_traces,
        failures: sorted_strings(failures.into_iter()),
        receipt,
    }
}

fn trace_group_passes(shadow: &[ShadowTraceResult], ids: &BTreeSet<&str>) -> bool {
    !ids.is_empty()
        && ids.iter().all(|id| {
            shadow
                .iter()
                .find(|trace| trace.trace_id == *id)
                .is_some_and(|trace| trace.pass)
        })
}

fn render_skill_markdown(candidate: &WorkflowCandidate, skill: &SkillCandidateArtifact) -> String {
    let mut out = String::new();
    out.push_str("---\n");
    push_yaml_string(&mut out, "name", &skill.name);
    push_yaml_string(&mut out, "short", &skill.short);
    push_yaml_string(&mut out, "description", &skill.description);
    push_yaml_string(&mut out, "when_to_use", &skill.when_to_use);
    push_yaml_list(&mut out, "allowed_tools", &skill.allowed_tools);
    push_yaml_list(&mut out, "paths", &skill.paths);
    out.push_str("user_invocable: false\n");
    out.push_str("---\n\n");

    out.push_str("# ");
    out.push_str(&skill.name);
    out.push_str("\n\n");
    out.push_str("## Scope\n\n");
    out.push_str("Use this skill only for tasks matching the activation metadata and the replay evidence below. Do not load it as global guidance, and do not copy trace-specific values unless the current task supplies the same parameter.\n\n");

    out.push_str("## Replay Gate\n\n");
    out.push_str(&format!(
        "- decision: {}\n- source traces: {}\n- held-out sibling traces: {}\n- compared traces: {}\n",
        skill.replay_gate.receipt.decision,
        skill.replay_gate.original_trace_count,
        skill.replay_gate.heldout_trace_count,
        skill.replay_gate.compared_trace_count,
    ));
    if !skill.replay_gate.failures.is_empty() {
        out.push_str("- failures:\n");
        for failure in &skill.replay_gate.failures {
            out.push_str(&format!("  - {}\n", markdown_line(failure)));
        }
    }
    out.push('\n');

    out.push_str("## Evidence\n\n");
    for evidence in &skill.evidence_refs {
        out.push_str(&format!(
            "- {}: `{}` hash `{}` actions `{}`\n",
            evidence_role_label(&evidence.role),
            markdown_line(&evidence.trace_id),
            markdown_line(&evidence.source_hash),
            markdown_line(&evidence.action_ids.join(", ")),
        ));
    }
    out.push('\n');

    out.push_str("## Procedure\n\n");
    for step in &candidate.steps {
        let segment = if step.segment == SegmentKind::Fuzzy {
            "review/LLM"
        } else {
            "deterministic"
        };
        out.push_str(&format!(
            "{}. `{}` `{}` ({segment})",
            step.index,
            markdown_line(&step.kind),
            markdown_line(&step.name)
        ));
        if !step.parameter_refs.is_empty() {
            out.push_str(&format!(
                "; parameterize `{}`",
                markdown_line(&step.parameter_refs.join("`, `"))
            ));
        }
        out.push('\n');
        if step
            .approval
            .as_ref()
            .is_some_and(|approval| approval.required)
        {
            out.push_str("   Preserve the recorded approval boundary before this step.\n");
        }
        if !step.required_secrets.is_empty() {
            out.push_str(&format!(
                "   Require logical secret id(s): `{}`.\n",
                markdown_line(&step.required_secrets.join("`, `"))
            ));
        }
    }
    out.push('\n');

    out.push_str("## Generalization Rules\n\n");
    out.push_str("- Generalize parameter names and step intent; do not memorize repository names, branches, ids, timestamps, or outputs from the evidence traces.\n");
    out.push_str("- Keep side-effect, secret, and approval boundaries at least as strict as the source workflow candidate.\n");
    out.push_str("- Prefer existing Harn workflows, stdlib helpers, and host capabilities over new host glue.\n");
    out
}

fn push_yaml_string(out: &mut String, key: &str, value: &str) {
    out.push_str(key);
    out.push_str(": ");
    out.push_str(&yaml_double_quote(value));
    out.push('\n');
}

fn push_yaml_list(out: &mut String, key: &str, values: &[String]) {
    if values.is_empty() {
        out.push_str(key);
        out.push_str(": []\n");
        return;
    }
    out.push_str(key);
    out.push_str(":\n");
    for value in values {
        out.push_str("  - ");
        out.push_str(&yaml_double_quote(value));
        out.push('\n');
    }
}

fn yaml_double_quote(value: &str) -> String {
    let escaped = value
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n");
    format!("\"{escaped}\"")
}

fn markdown_line(value: &str) -> String {
    value.replace('\n', " ")
}

fn evidence_role_label(role: &SkillCandidateEvidenceRole) -> &'static str {
    match role {
        SkillCandidateEvidenceRole::Source => "source",
        SkillCandidateEvidenceRole::HeldOut => "held-out",
    }
}

#[cfg(test)]
mod tests {
    use super::looks_like_path;

    #[test]
    fn skill_activation_paths_exclude_machine_local_paths() {
        assert!(looks_like_path("crates/harn-vm/src/lib.rs"));
        assert!(looks_like_path("docs/**"));
        assert!(looks_like_path("*.harn"));
        assert!(!looks_like_path("/Users/example/project/src/lib.rs"));
        assert!(!looks_like_path("~/projects/harn/src/lib.rs"));
        assert!(!looks_like_path("C:\\Users\\example\\project\\src\\lib.rs"));
        assert!(!looks_like_path("https://example.com/src/lib.rs"));
    }
}
