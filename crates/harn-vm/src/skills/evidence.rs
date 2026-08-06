//! Structured skill activation evidence.
//!
//! One host-consumable payload describing, per skill, whether its short
//! card was shown, why it was omitted, what it cost against the catalog
//! budget, and where it sits in the body lifecycle — so a host can render
//! `eligible` / `shown` / `omitted` / `loaded` / `used` without parsing the
//! catalog prompt text.
//!
//! The budget-fit decision is shared with the prompt renderer through
//! [`fit_catalog`]: `render_catalog` (see `crate::stdlib::skills`) and the
//! evidence builder call the same primitive, so the evidence can never
//! disagree with the catalog the model actually saw.

use crate::orchestration::estimate_chunk_tokens;

/// Bumped when the payload shape changes in a way hosts must notice.
pub const SKILL_ACTIVATION_EVIDENCE_SCHEMA_VERSION: u32 = 1;

/// Header emitted before the rendered catalog cards. Shared with the prompt
/// renderer so the budget accounting matches byte-for-byte.
pub const CATALOG_HEADER: &str = concat!(
    "## Available skills\n\n",
    "These skills are available. Call `load_skill({ name: \"<skill-id>\" })` to load the full body of a skill when it becomes relevant.\n\n",
);

/// Where a skill sits in the disclosure lifecycle. Registry-derived states
/// (`Eligible`/`Shown`/`Omitted`) are computed from the catalog fit; runtime
/// states (`Loaded`/`Used`) are folded in from lifecycle evidence the host
/// already tracks (the `skill.loaded` / `skill_activated` events).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkillBodyLifecycle {
    /// In the registry and model-invocable, but its short card was not part
    /// of the rendered catalog this turn.
    Eligible,
    /// Its short card was rendered into the catalog prompt.
    Shown,
    /// Eligible but its card was dropped before the model saw it.
    Omitted,
    /// The full body was pulled via `load_skill`.
    Loaded,
    /// The loaded body drove at least one downstream action.
    Used,
}

impl SkillBodyLifecycle {
    pub fn label(self) -> &'static str {
        match self {
            SkillBodyLifecycle::Eligible => "eligible",
            SkillBodyLifecycle::Shown => "shown",
            SkillBodyLifecycle::Omitted => "omitted",
            SkillBodyLifecycle::Loaded => "loaded",
            SkillBodyLifecycle::Used => "used",
        }
    }
}

/// Why an eligible skill's card was not rendered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkillOmittedReason {
    /// Dropped to keep the rendered catalog within its character budget.
    Budget,
    /// Trimmed before rendering because the catalog limit was already full.
    Limit,
    /// `disable-model-invocation` — never offered to the model, only
    /// user/direct invocation.
    DisableModelInvocation,
}

impl SkillOmittedReason {
    pub fn label(self) -> &'static str {
        match self {
            SkillOmittedReason::Budget => "budget",
            SkillOmittedReason::Limit => "catalog_limit",
            SkillOmittedReason::DisableModelInvocation => "disable_model_invocation",
        }
    }
}

/// Matched evidence for a candidate — score plus the human-readable trigger,
/// promoted so a host does not re-derive it from prompt text.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SkillMatchEvidence {
    pub score: f64,
    pub reason: String,
}

/// One skill's card input to the evidence builder. `block` is the fully
/// rendered short-card text; `in_catalog` marks entries that participate in
/// the budget fit (model-invocable and within the catalog limit).
#[derive(Debug, Clone)]
pub struct SkillCardInput {
    pub id: String,
    pub name: String,
    pub source: Option<String>,
    pub description: String,
    pub when_to_use: String,
    pub disable_model_invocation: bool,
    pub block: String,
    pub in_catalog: bool,
    pub matched: Option<SkillMatchEvidence>,
}

/// Per-skill activation evidence.
#[derive(Debug, Clone, PartialEq)]
pub struct SkillCardEvidence {
    pub id: String,
    pub name: String,
    pub source: Option<String>,
    pub description: String,
    pub when_to_use: String,
    pub disable_model_invocation: bool,
    pub selected: bool,
    pub omitted_reason: Option<SkillOmittedReason>,
    pub char_estimate: usize,
    pub token_estimate: usize,
    pub lifecycle: SkillBodyLifecycle,
    pub matched: Option<SkillMatchEvidence>,
}

/// The whole activation-evidence payload for one turn.
#[derive(Debug, Clone, PartialEq)]
pub struct SkillActivationEvidence {
    pub schema_version: u32,
    pub budget_chars: usize,
    pub used_chars: usize,
    pub budget_tokens: usize,
    pub used_tokens: usize,
    pub shown: Vec<String>,
    pub omitted: Vec<String>,
    pub cards: Vec<SkillCardEvidence>,
}

/// Result of fitting catalog blocks into a character budget.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogFit {
    /// The rendered catalog text (header + fitted cards + optional omission
    /// suffix).
    pub rendered: String,
    /// How many of `blocks` (from the front) were rendered.
    pub shown: usize,
}

/// Greedily fit `blocks` under `budget` characters below `header`, appending
/// a "N more skill(s) omitted" suffix when some were dropped. Callers pass a
/// non-empty `blocks`; both the prompt renderer and the evidence builder use
/// this so the shown/omitted split is decided in exactly one place.
pub fn fit_catalog(header: &str, blocks: &[String], budget: usize) -> CatalogFit {
    let omission_template = "\n\n... 1 more skill(s) omitted to stay within budget.";
    let budget = budget.max(header.len() + omission_template.len());

    let mut visible = 0usize;
    let mut rendered = String::from(header);
    while visible < blocks.len() {
        let candidate_len = rendered.len()
            + if visible == 0 {
                blocks[visible].len()
            } else {
                1 + blocks[visible].len()
            };
        if candidate_len > budget {
            break;
        }
        if visible > 0 {
            rendered.push('\n');
        }
        rendered.push_str(&blocks[visible]);
        visible += 1;
    }

    let mut omitted = blocks.len().saturating_sub(visible);
    if omitted > 0 {
        loop {
            let suffix = format!("\n\n... {omitted} more skill(s) omitted to stay within budget.");
            if rendered.len() + suffix.len() <= budget {
                rendered.push_str(&suffix);
                break;
            }
            if visible == 0 {
                break;
            }
            visible -= 1;
            omitted += 1;
            rendered = String::from(header);
            for (index, block) in blocks.iter().take(visible).enumerate() {
                if index > 0 {
                    rendered.push('\n');
                }
                rendered.push_str(block);
            }
        }
    }

    CatalogFit {
        rendered,
        shown: visible,
    }
}

/// Build the structured activation evidence for one turn.
///
/// `inputs` are every registry skill (already projected to catalog cards).
/// `budget` is the catalog character budget. `loaded` / `used` are the skill
/// ids the host has observed pulling a body / driving an action, so the same
/// payload records `eligible` vs `loaded` vs `used`.
pub fn build_activation_evidence(
    inputs: &[SkillCardInput],
    budget: usize,
    loaded: &[String],
    used: &[String],
) -> SkillActivationEvidence {
    // Blocks that participate in the budget fit, in the order they render.
    let mut fit_blocks: Vec<String> = Vec::new();
    let mut fit_input_index: Vec<usize> = Vec::new();
    for (index, input) in inputs.iter().enumerate() {
        if input.in_catalog {
            fit_blocks.push(input.block.clone());
            fit_input_index.push(index);
        }
    }

    let fit = if fit_blocks.is_empty() {
        CatalogFit {
            rendered: CATALOG_HEADER.to_string(),
            shown: 0,
        }
    } else {
        fit_catalog(CATALOG_HEADER, &fit_blocks, budget)
    };

    // The first `fit.shown` catalog blocks are the ones the model saw.
    let shown_inputs: std::collections::BTreeSet<usize> =
        fit_input_index.iter().take(fit.shown).copied().collect();

    let mut cards = Vec::with_capacity(inputs.len());
    let mut shown = Vec::new();
    let mut omitted = Vec::new();
    for (index, input) in inputs.iter().enumerate() {
        let (selected, omitted_reason) = if input.disable_model_invocation {
            (false, Some(SkillOmittedReason::DisableModelInvocation))
        } else if !input.in_catalog {
            (false, Some(SkillOmittedReason::Limit))
        } else if shown_inputs.contains(&index) {
            (true, None)
        } else {
            (false, Some(SkillOmittedReason::Budget))
        };

        // Registry-derived lifecycle, then override with runtime evidence.
        let mut lifecycle = if selected {
            SkillBodyLifecycle::Shown
        } else {
            SkillBodyLifecycle::Omitted
        };
        if used.iter().any(|id| id == &input.id) {
            lifecycle = SkillBodyLifecycle::Used;
        } else if loaded.iter().any(|id| id == &input.id) {
            lifecycle = SkillBodyLifecycle::Loaded;
        }

        if selected {
            shown.push(input.id.clone());
        } else {
            omitted.push(input.id.clone());
        }

        cards.push(SkillCardEvidence {
            id: input.id.clone(),
            name: input.name.clone(),
            source: input.source.clone(),
            description: input.description.clone(),
            when_to_use: input.when_to_use.clone(),
            disable_model_invocation: input.disable_model_invocation,
            selected,
            omitted_reason,
            char_estimate: input.block.len(),
            token_estimate: estimate_chunk_tokens(&input.block),
            lifecycle,
            matched: input.matched.clone(),
        });
    }

    let used_chars = fit.rendered.len();
    SkillActivationEvidence {
        schema_version: SKILL_ACTIVATION_EVIDENCE_SCHEMA_VERSION,
        budget_chars: budget,
        used_chars,
        budget_tokens: estimate_chunk_tokens_for_budget(budget),
        used_tokens: estimate_chunk_tokens(&fit.rendered),
        shown,
        omitted,
        cards,
    }
}

/// The budget expressed in tokens using the shared chars-per-token heuristic.
fn estimate_chunk_tokens_for_budget(budget_chars: usize) -> usize {
    budget_chars.div_ceil(4)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn card(id: &str, disable: bool, in_catalog: bool) -> SkillCardInput {
        SkillCardInput {
            id: id.to_string(),
            name: id.to_string(),
            source: Some("project".to_string()),
            description: format!("does {id}"),
            when_to_use: format!("when {id}"),
            disable_model_invocation: disable,
            block: format!("- `{id}`: does {id}\n  when: when {id}"),
            in_catalog: !disable && in_catalog,
            matched: None,
        }
    }

    #[test]
    fn shown_and_budget_omission_are_disjoint_and_ordered() {
        let inputs = vec![
            card("alpha", false, true),
            card("beta", false, true),
            card("gamma", false, true),
        ];
        // Budget fits the header + one card + the omission suffix, but not a
        // second card.
        let evidence = build_activation_evidence(&inputs, 260, &[], &[]);
        assert_eq!(evidence.shown, vec!["alpha".to_string()]);
        assert_eq!(
            evidence.omitted,
            vec!["beta".to_string(), "gamma".to_string()]
        );
        let alpha = &evidence.cards[0];
        assert!(alpha.selected);
        assert_eq!(alpha.lifecycle, SkillBodyLifecycle::Shown);
        assert!(alpha.omitted_reason.is_none());
        let beta = &evidence.cards[1];
        assert!(!beta.selected);
        assert_eq!(beta.omitted_reason, Some(SkillOmittedReason::Budget));
        assert_eq!(beta.lifecycle, SkillBodyLifecycle::Omitted);
    }

    #[test]
    fn disable_model_invocation_is_omitted_with_reason() {
        let inputs = vec![card("manual", true, false), card("auto", false, true)];
        let evidence = build_activation_evidence(&inputs, 10_000, &[], &[]);
        let manual = evidence.cards.iter().find(|c| c.id == "manual").unwrap();
        assert!(!manual.selected);
        assert_eq!(
            manual.omitted_reason,
            Some(SkillOmittedReason::DisableModelInvocation)
        );
        assert!(manual.disable_model_invocation);
        let auto = evidence.cards.iter().find(|c| c.id == "auto").unwrap();
        assert!(auto.selected);
    }

    #[test]
    fn over_limit_entries_report_limit_reason() {
        // `in_catalog=false` for a model-invocable skill marks a card trimmed
        // before render because the catalog limit was already full.
        let inputs = vec![card("kept", false, true), card("trimmed", false, false)];
        let evidence = build_activation_evidence(&inputs, 10_000, &[], &[]);
        let trimmed = evidence.cards.iter().find(|c| c.id == "trimmed").unwrap();
        assert_eq!(trimmed.omitted_reason, Some(SkillOmittedReason::Limit));
        assert!(!trimmed.disable_model_invocation);
    }

    #[test]
    fn runtime_lifecycle_overrides_registry_state() {
        let inputs = vec![card("alpha", false, true), card("beta", false, true)];
        let evidence = build_activation_evidence(
            &inputs,
            10_000,
            &["alpha".to_string()],
            &["beta".to_string()],
        );
        let alpha = evidence.cards.iter().find(|c| c.id == "alpha").unwrap();
        // Loaded overrides an otherwise-`shown` card.
        assert_eq!(alpha.lifecycle, SkillBodyLifecycle::Loaded);
        let beta = evidence.cards.iter().find(|c| c.id == "beta").unwrap();
        assert_eq!(beta.lifecycle, SkillBodyLifecycle::Used);
    }

    #[test]
    fn token_and_char_estimates_are_populated() {
        let inputs = vec![card("alpha", false, true)];
        let evidence = build_activation_evidence(&inputs, 10_000, &[], &[]);
        let alpha = &evidence.cards[0];
        assert!(alpha.char_estimate > 0);
        assert_eq!(alpha.token_estimate, alpha.char_estimate.div_ceil(4));
        assert!(evidence.used_chars >= CATALOG_HEADER.len());
        assert_eq!(evidence.budget_tokens, 10_000usize.div_ceil(4));
    }

    #[test]
    fn empty_catalog_yields_header_only_fit() {
        let inputs = vec![card("manual", true, false)];
        let evidence = build_activation_evidence(&inputs, 2000, &[], &[]);
        assert_eq!(evidence.used_chars, CATALOG_HEADER.len());
        assert!(evidence.shown.is_empty());
    }
}
