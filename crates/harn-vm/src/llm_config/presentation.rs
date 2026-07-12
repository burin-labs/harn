//! Catalog-authored presentation metadata for model pickers.
//!
//! Runtime routing does not consume these records. They let hosts render
//! recommendation presets and one- or two-dimensional model families without
//! hardcoding provider/model names in each UI.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct PresentationConfig {
    /// Stable recommendation id -> authored display copy and generic selector.
    #[serde(default)]
    pub variants: BTreeMap<String, PresentationVariantDef>,
    /// Stable presentation-family id -> ordered dimensions and presets.
    #[serde(default)]
    pub families: BTreeMap<String, ModelFamilyDef>,
}

impl PresentationConfig {
    pub fn is_empty(&self) -> bool {
        self.variants.is_empty() && self.families.is_empty()
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct PresentationVariantDef {
    /// Ascending display order in the resolved recommendation array.
    pub order: u16,
    pub label: String,
    pub description: String,
    pub selector: PresentationVariantSelector,
}

/// Small, deliberately closed selector vocabulary for global recommendations.
///
/// This keeps labels and choices in catalog data without growing a second
/// model-query language. The resolved artifact always contains a concrete
/// provider/model pair.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PresentationVariantSelector {
    Alias { name: String },
    Model { model_id: String },
    BestLocal,
    CheapestHosted,
    LargestVisionContext,
    LargestContext,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct ModelFamilyDef {
    pub label: String,
    pub plain_description: String,
    /// Concrete model for families without a model/variant dimension, such as
    /// a one-dimensional reasoning-effort family.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_id: Option<String>,
    pub dimensions: Vec<ModelFamilyDimensionDef>,
    pub presets: Vec<ModelFamilyPresetDef>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct ModelFamilyDimensionDef {
    pub key: String,
    pub label: String,
    pub plain_description: String,
    pub kind: ModelFamilyDimensionKind,
    pub ordered_values: Vec<ModelFamilyValueDef>,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ModelFamilyDimensionKind {
    /// Each value selects a concrete model id.
    Model,
    /// Each value is a provider reasoning-effort token.
    ReasoningEffort,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct ModelFamilyValueDef {
    pub value: String,
    pub label: String,
    pub plain_description: String,
    pub relative_cost_hint: u8,
    pub relative_speed_hint: u8,
    /// Required only for values on a `model` dimension.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_id: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct ModelFamilyPresetDef {
    pub id: String,
    pub label: String,
    pub plain_blurb: String,
    pub coordinates: BTreeMap<String, String>,
}
