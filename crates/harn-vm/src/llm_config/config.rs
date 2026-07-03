//! Aggregate provider/model catalog document: `ProvidersConfig`, its overlay
//! merge semantics, field-wise `[patch.models]` application, and the
//! tier/inference rule DTOs.
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};

use serde::Deserialize;

use super::*;

#[derive(Debug, Clone, Deserialize, Default)]
pub struct ProvidersConfig {
    #[serde(default)]
    pub default_provider: Option<String>,
    #[serde(default)]
    pub providers: BTreeMap<String, ProviderDef>,
    #[serde(default)]
    pub aliases: BTreeMap<String, AliasDef>,
    #[serde(default)]
    pub alias_tool_calling: BTreeMap<String, AliasToolCallingDef>,
    #[serde(default)]
    pub models: BTreeMap<String, ModelDef>,
    #[serde(default)]
    pub qc_defaults: BTreeMap<String, String>,
    #[serde(default)]
    pub inference_rules: Vec<InferenceRule>,
    #[serde(default)]
    pub tier_rules: Vec<TierRule>,
    #[serde(default)]
    pub tier_defaults: TierDefaults,
    #[serde(default)]
    pub model_defaults: BTreeMap<String, BTreeMap<String, toml::Value>>,
    #[serde(default)]
    pub model_roles: BTreeMap<String, BTreeMap<String, toml::Value>>,
    #[serde(default)]
    pub suppress: SuppressDef,
    #[serde(default)]
    pub patch: PatchDef,
}

/// Field-wise catalog patches applied on top of merged model rows.
///
/// Overlays have three complementary tools for adjusting the baseline
/// catalog, from coarsest to finest:
///
/// 1. **Whole-row replace** — `[models.<id>]` replaces the entire model row.
///    Use it to add a new route or when the overlay intentionally owns every
///    field of the row.
/// 2. **Field patch** — `[patch.models.<id>]` merges individual fields into
///    the existing row, leaving every unmentioned field at its baseline
///    value. Use it to tweak one knob (a `stream_timeout`, one pricing rate)
///    without copying the row verbatim and silently freezing the rest of its
///    fields against upstream catalog updates.
/// 3. **Route suppression** — `[suppress]` hides baseline routes from the
///    exported/served artifact entirely (see [`SuppressDef`]).
///
/// Patch semantics:
/// - Nested tables merge recursively; scalars **and arrays** replace the
///   base value wholesale (there is deliberately no per-element array merge).
/// - Within a single overlay, `[models.<id>]` whole-row replacement applies
///   BEFORE `[patch.models.<id>]`, so patch fields win over the same
///   overlay's whole-row fields.
/// - Patches are STICKY across layers: once accumulated, a patch re-applies
///   after every later layer's merge, including a later layer's whole-row
///   replacement of the same id. A patch means "always tweak this field",
///   not "tweak it once".
/// - A patch whose target row does not exist yet stays in the accumulator
///   silently and applies as soon as a later layer contributes the row;
///   [`ProvidersConfig::dangling_model_patches`] reports the leftovers for
///   doctor/export validation.
/// - A patch that produces a type-invalid row warns once per process and
///   keeps the unpatched row.
#[derive(Debug, Clone, Deserialize, Default, PartialEq)]
pub struct PatchDef {
    /// `[patch.models.<id>]` tables: partial `ModelDef` field sets merged
    /// field-wise into the model row with the same catalog id.
    #[serde(default)]
    pub models: BTreeMap<String, toml::Value>,
}

/// Routes hidden from the exported/served provider catalog artifact.
///
/// Lets an overlay drop baseline routes that are broken or unusable for the
/// embedding product (e.g. a dedicated-only serving route, or a local image
/// with a broken server-side tool parser) without forking the baseline
/// catalog. Suppression is artifact-level presentation: it removes the model
/// row, its aliases, and any recommendation variant derived from it, but does
/// not block runtime resolution of an explicitly requested model id.
///
/// This is one of three overlay tools (see [`PatchDef`] for the full set):
/// whole-row `[models.<id>]` replacement, field-wise `[patch.models.<id>]`
/// patches, and `[suppress]` route suppression. Combined with whole-row
/// `models` replacement, suppression also expresses route renames: define
/// the row under the new id and suppress the old one.
#[derive(Debug, Clone, Deserialize, Default, PartialEq, Eq)]
pub struct SuppressDef {
    /// `"provider:model_id"` selectors. Split on the FIRST colon only —
    /// model ids may themselves contain colons (e.g. Ollama image tags such
    /// as `ollama:qwen3.6:35b-a3b-coding-nvfp4`). Entries without a colon
    /// match nothing.
    #[serde(default)]
    pub routes: Vec<String>,
}

impl ProvidersConfig {
    pub fn is_empty(&self) -> bool {
        self.default_provider.is_none()
            && self.providers.is_empty()
            && self.aliases.is_empty()
            && self.alias_tool_calling.is_empty()
            && self.models.is_empty()
            && self.qc_defaults.is_empty()
            && self.inference_rules.is_empty()
            && self.tier_rules.is_empty()
            && self.model_defaults.is_empty()
            && self.model_roles.is_empty()
            && self.suppress.routes.is_empty()
            && self.patch.models.is_empty()
            && self.tier_defaults.default == default_mid()
    }

    /// `[patch.models]` ids with no matching model row in the merged config.
    ///
    /// Dangling patches are not an error at merge time — the row may arrive
    /// from a later layer — but doctor/export surfaces can report leftovers
    /// so a typo'd id doesn't silently patch nothing.
    pub fn dangling_model_patches(&self) -> Vec<&str> {
        self.patch
            .models
            .keys()
            .filter(|id| !self.models.contains_key(*id))
            .map(String::as_str)
            .collect()
    }

    pub fn merge_from(&mut self, overlay: &ProvidersConfig) {
        for (name, provider) in &overlay.providers {
            match self.providers.get_mut(name) {
                Some(existing) => existing.merge_from(provider),
                None => {
                    self.providers.insert(name.clone(), provider.clone());
                }
            }
        }
        self.aliases.extend(overlay.aliases.clone());
        self.alias_tool_calling
            .extend(overlay.alias_tool_calling.clone());
        self.models.extend(overlay.models.clone());
        self.qc_defaults.extend(overlay.qc_defaults.clone());

        // `[patch.models]` field-wise patches. Two deliberate ordering rules
        // (see [`PatchDef`]):
        //   1. Within one overlay, the whole-row `models` replacement above
        //      lands first, then patches — so `[patch.models.X]` fields win
        //      over the same overlay's `[models.X]` row.
        //   2. Patches are sticky: the accumulator re-applies after EVERY
        //      layer's merge, so a later layer's whole-row replacement still
        //      gets earlier layers' field tweaks re-applied on top. A patch
        //      means "always tweak this field", not "tweak it once".
        // Per-id patches from later layers deep-merge into the accumulator
        // (later layer wins per field), so two layers patching different
        // fields of the same row both stay sticky.
        // Short-circuit when no layer has contributed a patch so existing
        // patch-free configs pay nothing here.
        if !overlay.patch.models.is_empty() || !self.patch.models.is_empty() {
            for (id, patch) in &overlay.patch.models {
                match self.patch.models.get_mut(id) {
                    Some(existing) => deep_merge_toml(existing, patch),
                    None => {
                        self.patch.models.insert(id.clone(), patch.clone());
                    }
                }
            }
            apply_model_patches(&mut self.models, &self.patch.models);
        }

        if overlay.default_provider.is_some() {
            self.default_provider = overlay.default_provider.clone();
        }

        if !overlay.inference_rules.is_empty() {
            let mut merged = overlay.inference_rules.clone();
            merged.extend(self.inference_rules.clone());
            self.inference_rules = merged;
        }

        if !overlay.tier_rules.is_empty() {
            let mut merged = overlay.tier_rules.clone();
            merged.extend(self.tier_rules.clone());
            self.tier_rules = merged;
        }

        if overlay.tier_defaults.default != default_mid() {
            self.tier_defaults = overlay.tier_defaults.clone();
        }

        for (pattern, defaults) in &overlay.model_defaults {
            self.model_defaults
                .entry(pattern.clone())
                .or_default()
                .extend(defaults.clone());
        }

        for (role, defaults) in &overlay.model_roles {
            self.model_roles
                .entry(role.clone())
                .or_default()
                .extend(defaults.clone());
        }

        for route in &overlay.suppress.routes {
            if !self.suppress.routes.contains(route) {
                self.suppress.routes.push(route.clone());
            }
        }
    }
}

/// Recursively merge `overlay` into `base`. Tables merge key-by-key; every
/// other value shape — scalars AND arrays — replaces the base value
/// wholesale. Replacing arrays instead of merging them is the documented
/// convention: there is no sane universal element-wise merge for lists like
/// `capabilities` or `strengths`, so a patch that names an array owns it.
fn deep_merge_toml(base: &mut toml::Value, overlay: &toml::Value) {
    match (base, overlay) {
        (toml::Value::Table(base_table), toml::Value::Table(overlay_table)) => {
            for (key, overlay_value) in overlay_table {
                match base_table.get_mut(key) {
                    Some(base_value) => deep_merge_toml(base_value, overlay_value),
                    None => {
                        base_table.insert(key.clone(), overlay_value.clone());
                    }
                }
            }
        }
        (base_slot, overlay_value) => *base_slot = overlay_value.clone(),
    }
}

/// True once a type-invalid `[patch.models]` entry has been reported.
/// Patches re-apply on every layer merge (stickiness), so an unconditional
/// eprintln would repeat the same diagnostic once per layer per process.
static MODEL_PATCH_TYPE_ERROR_WARNED: AtomicBool = AtomicBool::new(false);

/// Apply every accumulated `[patch.models]` entry to its matching model row.
///
/// Patch application is `ModelDef -> toml::Value -> deep merge -> ModelDef`,
/// so a patch can only express states the row schema can represent. Ids with
/// no matching row are skipped (see
/// [`ProvidersConfig::dangling_model_patches`]). A patch that produces a
/// type-invalid row warns once (matching the `read_external_config` eprintln
/// precedent) and keeps the unpatched row, so one bad overlay field can't
/// take out the whole catalog entry.
fn apply_model_patches(
    models: &mut BTreeMap<String, ModelDef>,
    patches: &BTreeMap<String, toml::Value>,
) {
    for (id, patch) in patches {
        let Some(base) = models.get(id) else {
            continue;
        };
        match patched_model_row(base, patch) {
            Ok(patched) => {
                models.insert(id.clone(), patched);
            }
            Err(error) => {
                if !MODEL_PATCH_TYPE_ERROR_WARNED.swap(true, Ordering::Relaxed) {
                    eprintln!(
                        "[llm_config] invalid [patch.models.\"{id}\"] overlay \
                         (keeping the unpatched row): {error}"
                    );
                }
            }
        }
    }
}

/// Produce the patched version of one model row, or a description of why the
/// patch does not typecheck against the row schema.
fn patched_model_row(base: &ModelDef, patch: &toml::Value) -> Result<ModelDef, String> {
    let mut value = toml::Value::try_from(base)
        .map_err(|error| format!("serialize base row for patching: {error}"))?;
    deep_merge_toml(&mut value, patch);
    ModelDef::deserialize(value).map_err(|error| error.to_string())
}

#[derive(Debug, Clone, Deserialize)]
pub struct InferenceRule {
    #[serde(default)]
    pub pattern: Option<String>,
    #[serde(default)]
    pub contains: Option<String>,
    #[serde(default)]
    pub exact: Option<String>,
    pub provider: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TierRule {
    #[serde(default)]
    pub pattern: Option<String>,
    #[serde(default)]
    pub contains: Option<String>,
    #[serde(default)]
    pub exact: Option<String>,
    pub tier: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TierDefaults {
    #[serde(default = "default_mid")]
    pub default: String,
}

impl Default for TierDefaults {
    fn default() -> Self {
        Self {
            default: default_mid(),
        }
    }
}

fn default_mid() -> String {
    "mid".to_string()
}
