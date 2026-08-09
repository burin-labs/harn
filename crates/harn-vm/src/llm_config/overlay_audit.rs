//! Redundancy audit for product-local `providers.toml` overlays.
//!
//! An overlay exists to say what a product needs *differently* from the
//! baseline catalog. Every entry that merely restates the baseline is a second
//! copy of a fact Harn already owns, so a pricing or context-window refresh in
//! Harn silently fails to reach the product until someone edits both repos.
//! The failure is quiet — the overlay wins, so the stale copy is what ships.
//!
//! This module answers, for one overlay entry at a time: *would deleting it
//! change anything?* The answer is computed by actually running the real merge
//! with and without the entry and comparing the merged [`ProvidersConfig`], so
//! the audit cannot drift away from [`ProvidersConfig::merge_from`] — there is
//! no second model of what merging means.
//!
//! Three findings, in descending order of how mechanical the fix is:
//!
//! - [`OverlayFindingKind::Redundant`] — delete the entry.
//! - [`OverlayFindingKind::Narrowable`] — replace a whole-row `[models.<id>]`
//!   with the smaller `[patch.models.<id>]` printed in the finding. The row
//!   then tracks upstream for every field the product does not care about.
//! - [`OverlayFindingKind::Dangling`] — the entry targets nothing in the
//!   catalog. Advisory only: the catalog is not the whole universe of routes.
//!   A `[model_defaults."qwen3.6:*"]` glob or a `[suppress]` entry can
//!   legitimately speak about routes that arrive later — Ollama image tags
//!   discovered at runtime, a locally installed GGUF, a stale route some
//!   upstream `/v1/models` listing still returns. Reporting those is useful;
//!   deleting them on the audit's word is not.
//!
//! A whole-row copy also *drops* every baseline field it forgot to restate —
//! `strengths`, `tier`, a deprecation marker, sometimes `wire_model` — because
//! the row replaces rather than merges. Nothing warns, and the loss looks
//! exactly like upstream never having the field. Both fixes above report those
//! fields as `restored`: switching to a patch hands them back. That makes the
//! shipped catalog *change*, so the finding says so rather than claiming the
//! fix is free ([`OverlayFinding::preserves_catalog`]).
//!
//! Order-sensitive rule lists (`[[inference_rules]]`, `[[tier_rules]]`) get
//! [`OverlayFindingKind::DuplicateOfBaseline`], also advisory: overlay rules
//! are *prepended*, so a duplicate still changes which rule wins for inputs
//! that also match some rule in between.
//!
//! [`OverlayFinding::is_actionable`] is the line between the two groups, and
//! `--check` fails on the actionable ones only. A gate may demand an edit only
//! where the audit can prove the edit safe from the merge itself; everything
//! else is a report for a human.

use std::collections::BTreeMap;

use harn_glob::match_name as glob_match;
use serde::Serialize;

use super::catalog::is_logical_model_selector;
use super::{ModelDef, ProvidersConfig};

/// One overlay entry that no longer earns its keep.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct OverlayFinding {
    /// Section as an operator writes it in TOML, e.g. `models` or
    /// `patch.models`.
    pub section: &'static str,
    /// Entry key within the section: a map key, a `[[list]]` index, or the
    /// section name itself for single-valued sections.
    pub key: String,
    pub kind: OverlayFindingKind,
}

impl OverlayFinding {
    /// `section.key` as it would be written in TOML, for logs and CI output.
    /// Keys that are not bare TOML keys are quoted so the address can be
    /// pasted into a search.
    pub fn address(&self) -> String {
        if self.key == self.section {
            return self.section.to_string();
        }
        let bare = !self.key.is_empty()
            && self.key.chars().all(|character| {
                character.is_ascii_alphanumeric() || matches!(character, '_' | '-')
            });
        if bare {
            format!("{}.{}", self.section, self.key)
        } else {
            format!("{}.{:?}", self.section, self.key)
        }
    }

    /// True when the audit is asking for an edit, so `--check` should fail.
    ///
    /// This is not the same as the fix being free — see
    /// [`Self::preserves_catalog`]. A row that drops baseline fields is
    /// actionable precisely because the drop is silent and almost never
    /// intended.
    pub fn is_actionable(&self) -> bool {
        match self.kind {
            OverlayFindingKind::Redundant { .. } | OverlayFindingKind::Narrowable { .. } => true,
            OverlayFindingKind::Dangling { .. }
            | OverlayFindingKind::DuplicateOfBaseline { .. } => false,
        }
    }

    /// True when applying the fix leaves the shipped catalog byte-identical.
    /// False means the fix restores baseline fields the overlay was dropping,
    /// which is a deliberate change worth a human's eye.
    pub fn preserves_catalog(&self) -> bool {
        self.restored_fields().is_some_and(<[String]>::is_empty)
    }

    /// Baseline fields the suggested fix hands back, when the finding has a
    /// suggested fix at all.
    pub fn restored_fields(&self) -> Option<&[String]> {
        match &self.kind {
            OverlayFindingKind::Redundant { restored_fields }
            | OverlayFindingKind::Narrowable {
                restored_fields, ..
            } => Some(restored_fields),
            OverlayFindingKind::Dangling { .. } => Some(&[]),
            OverlayFindingKind::DuplicateOfBaseline { .. } => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "finding", rename_all = "snake_case")]
pub enum OverlayFindingKind {
    /// Delete the entry: the merged catalog is unchanged, apart from
    /// `restored_fields` coming back from the baseline.
    Redundant {
        /// Dotted field paths the entry was dropping. Empty means deleting it
        /// changes nothing at all.
        restored_fields: Vec<String>,
    },
    /// A whole-row `[models.<id>]` replacement that a strictly smaller
    /// `[patch.models.<id>]` reproduces. Verified by re-running the merge with
    /// the substitution, not inferred from a field diff.
    Narrowable {
        /// Ready-to-paste `[patch.models.<id>]` body.
        patch_toml: String,
        /// Fields the whole row restates from the baseline, which the patch
        /// hands back to upstream.
        inherited_fields: Vec<String>,
        /// Dotted field paths the whole row was dropping, which the patch
        /// restores.
        restored_fields: Vec<String>,
    },
    /// The entry names a target absent from the merged catalog. Advisory: the
    /// catalog is not the whole universe of routes an overlay may speak about
    /// — see the module docs.
    Dangling { target: String },
    /// An order-sensitive rule the baseline already declares. Advisory: see
    /// the module docs on prepend semantics.
    DuplicateOfBaseline { baseline_index: usize },
}

/// Audit `overlay` against `baseline`, returning findings in section order.
///
/// `baseline` is the catalog the overlay lands on — for a product overlay that
/// is [`super::embedded_config(None)`].
pub fn audit_overlay(baseline: &ProvidersConfig, overlay: &ProvidersConfig) -> Vec<OverlayFinding> {
    let merged = merge(baseline, overlay);
    let mut findings = Vec::new();
    for section in SECTIONS {
        for key in (section.keys)(overlay) {
            findings.extend(audit_entry(baseline, overlay, &merged, section, key));
        }
    }
    findings
}

/// Remove every entry [`SECTIONS`] knows how to address.
///
/// The result is empty exactly when the section table covers the whole
/// [`ProvidersConfig`] shape, which is what
/// `overlay_audit_tests::every_config_section_is_auditable` asserts: a section
/// the audit cannot address is a section whose entries can never be reported.
#[cfg(test)]
pub(super) fn strip_all_auditable_entries(overlay: &ProvidersConfig) -> ProvidersConfig {
    let mut stripped = overlay.clone();
    for section in SECTIONS {
        for key in (section.keys)(overlay) {
            (section.remove)(&mut stripped, &key);
        }
    }
    stripped
}

/// Classify one entry. At most one finding per entry: redundancy subsumes
/// narrowing (an entry that changes nothing needs no patch), and a dangling
/// entry that also happens to be redundant is more usefully reported as
/// dangling because it names the vanished target.
fn audit_entry(
    baseline: &ProvidersConfig,
    overlay: &ProvidersConfig,
    merged: &ProvidersConfig,
    section: &SectionSpec,
    key: String,
) -> Option<OverlayFinding> {
    let finding = |kind| {
        Some(OverlayFinding {
            section: section.name,
            key: key.clone(),
            kind,
        })
    };

    if let Some(target) = (section.dangling_target)(merged, &key) {
        return finding(OverlayFindingKind::Dangling { target });
    }
    if merge(baseline, &without(overlay, section, &key)) == *merged {
        return finding(OverlayFindingKind::Redundant {
            restored_fields: Vec::new(),
        });
    }
    if let Some(index) = (section.baseline_duplicate)(baseline, overlay, &key) {
        return finding(OverlayFindingKind::DuplicateOfBaseline {
            baseline_index: index,
        });
    }
    if section.name == MODELS_SECTION {
        if let Some(kind) = audit_model_row(baseline, overlay, merged, &key) {
            return finding(kind);
        }
    }
    None
}

/// Classify a whole-row `[models.<id>]` that shadows a baseline row.
///
/// The candidate replacement is the field-wise diff against the baseline: an
/// empty diff means the row can simply go, otherwise it becomes
/// `[patch.models.<id>]`. Either way the suggestion is accepted only after
/// re-running the merge with the substitution and confirming the catalog is
/// unchanged except on the fields the overlay was dropping. That check is what
/// makes the suggestion safe to apply: nothing here is inferred from the diff
/// alone.
fn audit_model_row(
    baseline: &ProvidersConfig,
    overlay: &ProvidersConfig,
    merged: &ProvidersConfig,
    id: &str,
) -> Option<OverlayFindingKind> {
    let baseline_row = row_table(baseline.models.get(id)?)?;
    let overlay_row = row_table(overlay.models.get(id)?)?;
    let delta = row_delta(&baseline_row, &overlay_row);
    if !delta.patch.is_empty() && delta.patch.len() >= overlay_row.len() {
        return None;
    }

    let mut rewritten = overlay.clone();
    rewritten.models.remove(id);
    if !delta.patch.is_empty() {
        rewritten
            .patch
            .models
            .insert(id.to_string(), toml::Value::Table(delta.patch.clone()));
    }
    if !catalogs_agree_except(&merge(baseline, &rewritten), merged, id, &delta.restored) {
        return None;
    }

    if delta.patch.is_empty() {
        return Some(OverlayFindingKind::Redundant {
            restored_fields: delta.restored,
        });
    }
    let inherited_fields = overlay_row
        .keys()
        .filter(|field| !delta.patch.contains_key(*field))
        .cloned()
        .collect();
    Some(OverlayFindingKind::Narrowable {
        patch_toml: render_patch(id, &delta.patch),
        inherited_fields,
        restored_fields: delta.restored,
    })
}

/// How a whole-row overlay copy differs from the baseline row it shadows.
struct RowDelta {
    /// Fields the overlay row actually changes, shaped as a
    /// `[patch.models.<id>]` body.
    patch: toml::Table,
    /// Dotted paths the overlay row omits that the baseline sets. Whole-row
    /// replacement drops these; a patch cannot express dropping them, so
    /// switching hands them back to upstream.
    restored: Vec<String>,
}

fn row_delta(baseline_row: &toml::Table, overlay_row: &toml::Table) -> RowDelta {
    let mut delta = RowDelta {
        patch: toml::Table::new(),
        restored: Vec::new(),
    };
    collect_row_delta("", baseline_row, overlay_row, &mut delta);
    delta
}

/// Recurse into nested tables so a single changed pricing rate does not drag
/// the whole pricing table into the patch, and so a cache rate the overlay
/// forgot is reported at `pricing.cache_read_per_mtok` rather than as a
/// wholesale `pricing` replacement.
fn collect_row_delta(
    prefix: &str,
    baseline_row: &toml::Table,
    overlay_row: &toml::Table,
    delta: &mut RowDelta,
) {
    for key in baseline_row.keys() {
        if !overlay_row.contains_key(key) {
            delta.restored.push(format!("{prefix}{key}"));
        }
    }
    for (key, overlay_value) in overlay_row {
        match (baseline_row.get(key), overlay_value) {
            (Some(baseline_value), _) if baseline_value == overlay_value => {}
            (Some(toml::Value::Table(baseline_table)), toml::Value::Table(overlay_table)) => {
                let mut nested = RowDelta {
                    patch: toml::Table::new(),
                    restored: Vec::new(),
                };
                collect_row_delta(
                    &format!("{prefix}{key}."),
                    baseline_table,
                    overlay_table,
                    &mut nested,
                );
                if !nested.patch.is_empty() {
                    delta
                        .patch
                        .insert(key.clone(), toml::Value::Table(nested.patch));
                }
                delta.restored.extend(nested.restored);
            }
            _ => {
                delta.patch.insert(key.clone(), overlay_value.clone());
            }
        }
    }
}

/// True when two merged catalogs agree everywhere except on `restored` paths
/// of one model row — the fields the rewrite deliberately hands back.
fn catalogs_agree_except(
    rewritten: &ProvidersConfig,
    original: &ProvidersConfig,
    id: &str,
    restored: &[String],
) -> bool {
    if restored.is_empty() {
        return rewritten == original;
    }
    let (Some(rewritten_row), Some(original_row)) =
        (rewritten.models.get(id), original.models.get(id))
    else {
        return false;
    };
    let mut rewritten_rest = rewritten.clone();
    let mut original_rest = original.clone();
    rewritten_rest.models.remove(id);
    original_rest.models.remove(id);
    if rewritten_rest != original_rest {
        return false;
    }
    let (Some(mut left), Some(mut right)) = (row_table(rewritten_row), row_table(original_row))
    else {
        return false;
    };
    for path in restored {
        remove_path(&mut left, path);
        remove_path(&mut right, path);
    }
    left == right
}

fn remove_path(table: &mut toml::Table, path: &str) {
    match path.split_once('.') {
        None => {
            table.remove(path);
        }
        Some((head, rest)) => {
            if let Some(toml::Value::Table(child)) = table.get_mut(head) {
                remove_path(child, rest);
            }
        }
    }
}

fn row_table(row: &ModelDef) -> Option<toml::Table> {
    match toml::Value::try_from(row).ok()? {
        toml::Value::Table(table) => Some(table),
        _ => None,
    }
}

fn render_patch(id: &str, patch: &toml::Table) -> String {
    let mut models = toml::Table::new();
    models.insert(id.to_string(), toml::Value::Table(patch.clone()));
    let mut patch_section = toml::Table::new();
    patch_section.insert("models".to_string(), toml::Value::Table(models));
    let mut document = toml::Table::new();
    document.insert("patch".to_string(), toml::Value::Table(patch_section));
    toml::to_string_pretty(&document)
        .unwrap_or_else(|error| format!("# failed to render patch: {error}\n"))
}

/// The catalog this overlay produces, with the `[patch.models]` accumulator
/// cleared.
///
/// Patches have already been applied to the model rows by the time
/// `merge_from` returns; what stays in the accumulator only matters to layers
/// that merge *after* this overlay, because a patch re-applies on top of a
/// later whole-row replacement. Comparing the accumulator would make every
/// question the audit asks unanswerable — dropping a patch entry always
/// changes it, and rewriting a whole row as a patch always changes it — while
/// the catalog the product actually ships stays identical. So the audit
/// compares the catalog and leaves stickiness out of the equality.
///
/// The trade is real and worth stating: narrowing a `[models.<id>]` row to
/// `[patch.models.<id>]` makes that field survive a later layer's whole-row
/// replacement of the same id (a user's `~/.config/harn/providers.toml`),
/// where the whole-row form would lose to it. For a product overlay asserting
/// a product invariant, surviving is the behaviour you want.
fn merge(baseline: &ProvidersConfig, overlay: &ProvidersConfig) -> ProvidersConfig {
    let mut merged = baseline.clone();
    merged.merge_from(overlay);
    merged.patch.models.clear();
    merged
}

fn without(overlay: &ProvidersConfig, section: &SectionSpec, key: &str) -> ProvidersConfig {
    let mut trimmed = overlay.clone();
    (section.remove)(&mut trimmed, key);
    trimmed
}

const MODELS_SECTION: &str = "models";

/// Per-section knowledge the audit needs, kept as data so adding a section to
/// [`ProvidersConfig`] is one row here rather than a new branch in three
/// functions. `overlay_audit_tests::every_config_section_is_auditable` fails
/// when a section is missing.
struct SectionSpec {
    name: &'static str,
    /// Entry keys this overlay contributes to the section.
    keys: fn(&ProvidersConfig) -> Vec<String>,
    /// Drop one entry from a mutable overlay clone.
    remove: fn(&mut ProvidersConfig, &str),
    /// The target this entry references, when the merged catalog lacks it.
    dangling_target: fn(&ProvidersConfig, &str) -> Option<String>,
    /// Index of an identical baseline element, for order-sensitive lists.
    baseline_duplicate: fn(&ProvidersConfig, &ProvidersConfig, &str) -> Option<usize>,
}

fn no_dangling_target(_merged: &ProvidersConfig, _key: &str) -> Option<String> {
    None
}

fn no_baseline_duplicate(
    _baseline: &ProvidersConfig,
    _overlay: &ProvidersConfig,
    _key: &str,
) -> Option<usize> {
    None
}

/// Every removable unit of a `providers.toml` overlay.
const SECTIONS: &[SectionSpec] = &[
    SectionSpec {
        name: "default_provider",
        keys: |overlay| singleton("default_provider", overlay.default_provider.is_some()),
        remove: |overlay, _| overlay.default_provider = None,
        dangling_target: no_dangling_target,
        baseline_duplicate: no_baseline_duplicate,
    },
    SectionSpec {
        name: "providers",
        keys: |overlay| map_keys(&overlay.providers),
        remove: |overlay, key| {
            overlay.providers.remove(key);
        },
        dangling_target: no_dangling_target,
        baseline_duplicate: no_baseline_duplicate,
    },
    SectionSpec {
        name: "aliases",
        keys: |overlay| map_keys(&overlay.aliases),
        remove: |overlay, key| {
            overlay.aliases.remove(key);
        },
        dangling_target: no_dangling_target,
        baseline_duplicate: no_baseline_duplicate,
    },
    SectionSpec {
        name: "alias_tool_calling",
        keys: |overlay| map_keys(&overlay.alias_tool_calling),
        remove: |overlay, key| {
            overlay.alias_tool_calling.remove(key);
        },
        // Keyed by alias name: tool-calling policy for an alias nothing
        // resolves is inert.
        dangling_target: |merged, key| {
            (!merged.aliases.contains_key(key)).then(|| format!("alias {key}"))
        },
        baseline_duplicate: no_baseline_duplicate,
    },
    SectionSpec {
        name: MODELS_SECTION,
        keys: |overlay| map_keys(&overlay.models),
        remove: |overlay, key| {
            overlay.models.remove(key);
        },
        dangling_target: no_dangling_target,
        baseline_duplicate: no_baseline_duplicate,
    },
    SectionSpec {
        name: "patch.models",
        keys: |overlay| map_keys(&overlay.patch.models),
        remove: |overlay, key| {
            overlay.patch.models.remove(key);
        },
        dangling_target: |merged, key| {
            (!merged.models.contains_key(key)).then(|| format!("model {key}"))
        },
        baseline_duplicate: no_baseline_duplicate,
    },
    SectionSpec {
        name: "qc_defaults",
        keys: |overlay| map_keys(&overlay.qc_defaults),
        remove: |overlay, key| {
            overlay.qc_defaults.remove(key);
        },
        dangling_target: no_dangling_target,
        baseline_duplicate: no_baseline_duplicate,
    },
    SectionSpec {
        name: "model_defaults",
        keys: |overlay| map_keys(&overlay.model_defaults),
        remove: |overlay, key| {
            overlay.model_defaults.remove(key);
        },
        dangling_target: |merged, key| {
            (!matches_any_route(merged, key)).then(|| format!("route pattern {key}"))
        },
        baseline_duplicate: no_baseline_duplicate,
    },
    SectionSpec {
        name: "model_roles",
        keys: |overlay| map_keys(&overlay.model_roles),
        remove: |overlay, key| {
            overlay.model_roles.remove(key);
        },
        dangling_target: no_dangling_target,
        baseline_duplicate: no_baseline_duplicate,
    },
    SectionSpec {
        name: "model_ladders",
        keys: |overlay| map_keys(&overlay.model_ladders),
        remove: |overlay, key| {
            overlay.model_ladders.remove(key);
        },
        dangling_target: no_dangling_target,
        baseline_duplicate: no_baseline_duplicate,
    },
    SectionSpec {
        name: "presentation.variants",
        keys: |overlay| map_keys(&overlay.presentation.variants),
        remove: |overlay, key| {
            overlay.presentation.variants.remove(key);
        },
        dangling_target: no_dangling_target,
        baseline_duplicate: no_baseline_duplicate,
    },
    SectionSpec {
        name: "presentation.families",
        keys: |overlay| map_keys(&overlay.presentation.families),
        remove: |overlay, key| {
            overlay.presentation.families.remove(key);
        },
        dangling_target: no_dangling_target,
        baseline_duplicate: no_baseline_duplicate,
    },
    SectionSpec {
        name: "suppress.routes",
        keys: |overlay| overlay.suppress.routes.clone(),
        remove: |overlay, key| overlay.suppress.routes.retain(|route| route != key),
        dangling_target: |merged, key| {
            (!suppresses_a_route(merged, key)).then(|| format!("route {key}"))
        },
        baseline_duplicate: no_baseline_duplicate,
    },
    SectionSpec {
        name: "tier_defaults",
        keys: |overlay| {
            singleton(
                "tier_defaults",
                overlay.tier_defaults != super::TierDefaults::default(),
            )
        },
        remove: |overlay, _| overlay.tier_defaults = super::TierDefaults::default(),
        dangling_target: no_dangling_target,
        baseline_duplicate: no_baseline_duplicate,
    },
    SectionSpec {
        name: "inference_rules",
        keys: |overlay| indices(overlay.inference_rules.len()),
        remove: |overlay, key| remove_at(&mut overlay.inference_rules, key),
        dangling_target: no_dangling_target,
        baseline_duplicate: |baseline, overlay, key| {
            index_of_equal(&baseline.inference_rules, &overlay.inference_rules, key)
        },
    },
    SectionSpec {
        name: "tier_rules",
        keys: |overlay| indices(overlay.tier_rules.len()),
        remove: |overlay, key| remove_at(&mut overlay.tier_rules, key),
        dangling_target: no_dangling_target,
        baseline_duplicate: |baseline, overlay, key| {
            index_of_equal(&baseline.tier_rules, &overlay.tier_rules, key)
        },
    },
];

fn map_keys<V>(map: &BTreeMap<String, V>) -> Vec<String> {
    map.keys().cloned().collect()
}

fn singleton(name: &'static str, present: bool) -> Vec<String> {
    if present {
        vec![name.to_string()]
    } else {
        Vec::new()
    }
}

fn indices(len: usize) -> Vec<String> {
    (0..len).map(|index| index.to_string()).collect()
}

fn remove_at<T>(items: &mut Vec<T>, key: &str) {
    if let Ok(index) = key.parse::<usize>() {
        if index < items.len() {
            items.remove(index);
        }
    }
}

fn index_of_equal<T: PartialEq>(baseline: &[T], overlay: &[T], key: &str) -> Option<usize> {
    let entry = overlay.get(key.parse::<usize>().ok()?)?;
    baseline.iter().position(|candidate| candidate == entry)
}

/// True when a `[model_defaults.<pattern>]` glob still names a live route.
/// `logical:` selectors are exact publisher contracts validated by
/// [`super::model_default_issues`], so they are not this audit's business.
fn matches_any_route(merged: &ProvidersConfig, pattern: &str) -> bool {
    if is_logical_model_selector(pattern) {
        return true;
    }
    merged.models.iter().any(|(id, model)| {
        route_identities(id, model).any(|identity| glob_match(pattern, &identity))
    })
}

/// Every spelling of a route a `model_defaults` pattern may be written
/// against, mirroring the identities [`super::model_params_for_route`] tries.
fn route_identities<'a>(id: &'a str, model: &'a ModelDef) -> impl Iterator<Item = String> + 'a {
    [
        Some(id.to_string()),
        Some(format!("{}/{id}", model.provider)),
        model.wire_model.clone(),
    ]
    .into_iter()
    .flatten()
}

/// `[suppress]` routes are `provider:model_id`, split on the FIRST colon only
/// because model ids carry colons (`ollama:qwen3.6:35b-...`). Suppression hides
/// both model rows and the aliases pointing at them, so an entry that only
/// covers an alias is still doing work.
fn suppresses_a_route(merged: &ProvidersConfig, route: &str) -> bool {
    let Some((provider, model_id)) = route.split_once(':') else {
        return false;
    };
    let hides_model = merged
        .models
        .get(model_id)
        .is_some_and(|model| model.provider == provider);
    hides_model
        || merged
            .aliases
            .values()
            .any(|alias| alias.provider == provider && alias.id == model_id)
}
