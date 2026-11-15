use serde::{Deserialize, Serialize};

/// Serializable goal data carried by a pin. Checks stay with the Harn caller;
/// a durable pin carries only the objective, criterion identities and limits.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GoalPinSpec {
    pub objective: String,
    pub success_criteria: Vec<GoalPinCriterion>,
    pub retired_criteria: Vec<GoalPinCriterion>,
    pub constraints: Vec<String>,
    pub budget: serde_json::Map<String, serde_json::Value>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GoalPinCriterion {
    pub id: String,
    pub description: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GoalPinProjection {
    pub spec: GoalPinSpec,
    pub lifecycle: GoalPinLifecycle,
    pub control_id: Option<String>,
    /// Canonical goal renderer output with only the repeated objective omitted.
    pub objective_covered_body: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_body: Option<i64>,
}

/// Rendering context cannot opt into managed retarget or completion policy.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GoalPinLifecycle {
    Managed,
    Context,
}

impl GoalPinProjection {
    pub(crate) fn from_json(value: serde_json::Value) -> Result<Box<Self>, String> {
        let pin: Self = serde_json::from_value(value).map_err(|error| error.to_string())?;
        if pin.spec.objective.trim().is_empty() {
            return Err("goal_pin.spec.objective must be non-blank".to_string());
        }
        if pin.objective_covered_body.trim().is_empty() {
            return Err("goal_pin.objective_covered_body must be non-blank".to_string());
        }
        if pin
            .spec
            .success_criteria
            .iter()
            .chain(&pin.spec.retired_criteria)
            .any(|criterion| criterion.id.is_empty() || criterion.description.trim().is_empty())
        {
            return Err("goal_pin criteria need non-empty ids and descriptions".to_string());
        }
        Ok(Box::new(pin))
    }
}
