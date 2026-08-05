use std::collections::BTreeSet;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;

const FLOAT_TOLERANCE: f64 = 1e-6;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ToolCallEvalCase {
    pub id: String,
    pub prompt: String,
    #[serde(default)]
    pub tools: Vec<ToolDef>,
    pub expected: ExpectedToolCall,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub baseline_pass_rate: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ToolDef {
    pub name: String,
    #[serde(default)]
    pub description: String,
    /// Harn tool parameter schema: a map of parameter name to JSON Schema
    /// fragments. `llm_call` wraps this into provider-native function schemas.
    #[serde(default)]
    pub parameters: JsonValue,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        rename = "outputSchema"
    )]
    pub output_schema: Option<JsonValue>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub namespace: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub defer_loading: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ExpectedToolCall {
    Exact {
        name: String,
        args: JsonValue,
    },
    Predicate {
        description: String,
        judge_prompt: String,
    },
    Refusal {
        reason_must_match: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ObservedToolCall {
    pub name: String,
    #[serde(default)]
    pub args: JsonValue,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ObservedToolCallOutcome {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call: Option<ObservedToolCall>,
    #[serde(default)]
    pub final_text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PredicateJudgeVerdict {
    pub passed: bool,
    #[serde(default)]
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ToolCallScore {
    pub passed: bool,
    pub reason: String,
}

#[derive(Debug)]
pub enum ToolCallEvalDatasetError {
    Io { path: PathBuf, message: String },
    Json { path: PathBuf, message: String },
    Validation { path: PathBuf, message: String },
}

impl fmt::Display for ToolCallEvalDatasetError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io { path, message } => write!(f, "{}: {message}", path.display()),
            Self::Json { path, message } => write!(f, "{}: {message}", path.display()),
            Self::Validation { path, message } => write!(f, "{}: {message}", path.display()),
        }
    }
}

impl std::error::Error for ToolCallEvalDatasetError {}

pub fn load_tool_call_eval_dataset(
    path: &Path,
) -> Result<Vec<ToolCallEvalCase>, ToolCallEvalDatasetError> {
    let mut cases = Vec::new();
    for file in tool_call_eval_case_files(path)? {
        let raw = fs::read_to_string(&file).map_err(|error| ToolCallEvalDatasetError::Io {
            path: file.clone(),
            message: error.to_string(),
        })?;
        let value: JsonValue =
            serde_json::from_str(&raw).map_err(|error| ToolCallEvalDatasetError::Json {
                path: file.clone(),
                message: error.to_string(),
            })?;
        let mut loaded = if value.is_array() {
            serde_json::from_value::<Vec<ToolCallEvalCase>>(value).map_err(|error| {
                ToolCallEvalDatasetError::Json {
                    path: file.clone(),
                    message: error.to_string(),
                }
            })?
        } else {
            vec![
                serde_json::from_value::<ToolCallEvalCase>(value).map_err(|error| {
                    ToolCallEvalDatasetError::Json {
                        path: file.clone(),
                        message: error.to_string(),
                    }
                })?,
            ]
        };
        for case in &loaded {
            validate_case(case, &file)?;
        }
        cases.append(&mut loaded);
    }
    cases.sort_by(|left, right| left.id.cmp(&right.id));
    validate_unique_case_ids(&cases, path)?;
    Ok(cases)
}

fn tool_call_eval_case_files(path: &Path) -> Result<Vec<PathBuf>, ToolCallEvalDatasetError> {
    if path.is_file() {
        return Ok(vec![path.to_path_buf()]);
    }
    let cases_dir = path.join("cases");
    let root = if cases_dir.is_dir() {
        cases_dir
    } else {
        path.to_path_buf()
    };
    let mut files = Vec::new();
    collect_json_files(&root, &mut files)?;
    files.sort();
    Ok(files)
}

fn collect_json_files(dir: &Path, out: &mut Vec<PathBuf>) -> Result<(), ToolCallEvalDatasetError> {
    let entries = fs::read_dir(dir).map_err(|error| ToolCallEvalDatasetError::Io {
        path: dir.to_path_buf(),
        message: error.to_string(),
    })?;
    for entry in entries {
        let entry = entry.map_err(|error| ToolCallEvalDatasetError::Io {
            path: dir.to_path_buf(),
            message: error.to_string(),
        })?;
        let path = entry.path();
        if path.is_dir() {
            collect_json_files(&path, out)?;
        } else if path.extension().is_some_and(|ext| ext == "json") {
            out.push(path);
        }
    }
    Ok(())
}

fn validate_case(case: &ToolCallEvalCase, path: &Path) -> Result<(), ToolCallEvalDatasetError> {
    if case.id.trim().is_empty() {
        return validation_error(path, "case id must not be empty");
    }
    if case.prompt.trim().is_empty() {
        return validation_error(path, format!("{}: prompt must not be empty", case.id));
    }
    let mut names = BTreeSet::new();
    for tool in &case.tools {
        if tool.name.trim().is_empty() {
            return validation_error(path, format!("{}: tool name must not be empty", case.id));
        }
        if !names.insert(tool.name.as_str()) {
            return validation_error(
                path,
                format!("{}: duplicate tool name `{}`", case.id, tool.name),
            );
        }
        if !tool.parameters.is_object() {
            return validation_error(
                path,
                format!(
                    "{}: tool `{}` parameters must be an object",
                    case.id, tool.name
                ),
            );
        }
    }
    if let ExpectedToolCall::Exact { name, .. } = &case.expected {
        if !names.contains(name.as_str()) {
            return validation_error(
                path,
                format!("{}: expected tool `{name}` is not declared", case.id),
            );
        }
    }
    if let Some(rate) = case.baseline_pass_rate {
        if !(0.0..=1.0).contains(&rate) {
            return validation_error(
                path,
                format!("{}: baseline_pass_rate must be in [0, 1]", case.id),
            );
        }
    }
    Ok(())
}

fn validation_error<T>(
    path: &Path,
    message: impl Into<String>,
) -> Result<T, ToolCallEvalDatasetError> {
    Err(ToolCallEvalDatasetError::Validation {
        path: path.to_path_buf(),
        message: message.into(),
    })
}

fn validate_unique_case_ids(
    cases: &[ToolCallEvalCase],
    path: &Path,
) -> Result<(), ToolCallEvalDatasetError> {
    let mut seen = BTreeSet::new();
    for case in cases {
        if !seen.insert(case.id.as_str()) {
            return validation_error(path, format!("duplicate case id `{}`", case.id));
        }
    }
    Ok(())
}

pub fn score_tool_call_case(
    case: &ToolCallEvalCase,
    observed: &ObservedToolCallOutcome,
    predicate_verdict: Option<&PredicateJudgeVerdict>,
) -> ToolCallScore {
    match &case.expected {
        ExpectedToolCall::Exact { name, args } => score_exact(name, args, observed),
        ExpectedToolCall::Predicate { .. } => match predicate_verdict {
            Some(verdict) => ToolCallScore {
                passed: verdict.passed,
                reason: if verdict.reason.is_empty() {
                    "predicate judge returned no reason".to_string()
                } else {
                    verdict.reason.clone()
                },
            },
            None => ToolCallScore {
                passed: false,
                reason: "predicate case was not judged".to_string(),
            },
        },
        ExpectedToolCall::Refusal { reason_must_match } => {
            score_refusal(reason_must_match, observed)
        }
    }
}

fn score_exact(name: &str, args: &JsonValue, observed: &ObservedToolCallOutcome) -> ToolCallScore {
    let Some(call) = observed.tool_call.as_ref() else {
        return ToolCallScore {
            passed: false,
            reason: format!("expected `{name}` tool call, observed no tool call"),
        };
    };
    if call.name != name {
        return ToolCallScore {
            passed: false,
            reason: format!("expected tool `{name}`, observed `{}`", call.name),
        };
    }
    if !json_deep_equal_with_numeric_tolerance(args, &call.args) {
        return ToolCallScore {
            passed: false,
            reason: format!("expected args {args}, observed {}", call.args),
        };
    }
    ToolCallScore {
        passed: true,
        reason: format!("matched `{name}` and canonical arguments"),
    }
}

fn score_refusal(pattern: &str, observed: &ObservedToolCallOutcome) -> ToolCallScore {
    if let Some(call) = observed.tool_call.as_ref() {
        return ToolCallScore {
            passed: false,
            reason: format!("expected refusal, observed tool `{}`", call.name),
        };
    }
    match regex::Regex::new(pattern) {
        Ok(regex) if regex.is_match(&observed.final_text) => ToolCallScore {
            passed: true,
            reason: "refusal text matched expected reason pattern".to_string(),
        },
        Ok(_) => ToolCallScore {
            passed: false,
            reason: format!(
                "refusal text did not match `{pattern}`: {}",
                observed.final_text
            ),
        },
        Err(error) => ToolCallScore {
            passed: false,
            reason: format!("invalid refusal regex `{pattern}`: {error}"),
        },
    }
}

pub fn json_deep_equal_with_numeric_tolerance(left: &JsonValue, right: &JsonValue) -> bool {
    match (left, right) {
        (JsonValue::Null, JsonValue::Null) => true,
        (JsonValue::Bool(left), JsonValue::Bool(right)) => left == right,
        (JsonValue::String(left), JsonValue::String(right)) => left == right,
        (JsonValue::Number(left), JsonValue::Number(right)) => {
            match (left.as_f64(), right.as_f64()) {
                (Some(left), Some(right)) => (left - right).abs() <= FLOAT_TOLERANCE,
                _ => left == right,
            }
        }
        (JsonValue::Array(left), JsonValue::Array(right)) => {
            left.len() == right.len()
                && left
                    .iter()
                    .zip(right)
                    .all(|(l, r)| json_deep_equal_with_numeric_tolerance(l, r))
        }
        (JsonValue::Object(left), JsonValue::Object(right)) => {
            left.len() == right.len()
                && left.iter().all(|(key, left_value)| {
                    right.get(key).is_some_and(|right_value| {
                        json_deep_equal_with_numeric_tolerance(left_value, right_value)
                    })
                })
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn exact_case() -> ToolCallEvalCase {
        ToolCallEvalCase {
            id: "exact".to_string(),
            prompt: "Add two numbers".to_string(),
            tools: vec![ToolDef {
                name: "add".to_string(),
                description: String::new(),
                parameters: json!({
                    "left": {"type": "integer"},
                    "right": {"type": "integer"}
                }),
                output_schema: None,
                namespace: None,
                defer_loading: None,
            }],
            expected: ExpectedToolCall::Exact {
                name: "add".to_string(),
                args: json!({"left": 2, "right": 3.0}),
            },
            baseline_pass_rate: None,
            source: None,
            tags: Vec::new(),
        }
    }

    #[test]
    fn exact_scoring_accepts_numeric_tolerance() {
        let score = score_tool_call_case(
            &exact_case(),
            &ObservedToolCallOutcome {
                tool_call: Some(ObservedToolCall {
                    name: "add".to_string(),
                    args: json!({"right": 3.0000001, "left": 2}),
                }),
                final_text: String::new(),
            },
            None,
        );
        assert!(score.passed, "{score:?}");
    }

    #[test]
    fn exact_scoring_rejects_extra_args() {
        let score = score_tool_call_case(
            &exact_case(),
            &ObservedToolCallOutcome {
                tool_call: Some(ObservedToolCall {
                    name: "add".to_string(),
                    args: json!({"left": 2, "right": 3, "extra": true}),
                }),
                final_text: String::new(),
            },
            None,
        );
        assert!(!score.passed);
        assert!(score.reason.contains("expected args"));
    }

    #[test]
    fn refusal_requires_no_tool_and_matching_text() {
        let case = ToolCallEvalCase {
            id: "refusal".to_string(),
            prompt: "Tell a joke".to_string(),
            tools: Vec::new(),
            expected: ExpectedToolCall::Refusal {
                reason_must_match: "(?i)not.*available".to_string(),
            },
            baseline_pass_rate: None,
            source: None,
            tags: Vec::new(),
        };
        let score = score_tool_call_case(
            &case,
            &ObservedToolCallOutcome {
                tool_call: None,
                final_text: "That tool is not available for this request.".to_string(),
            },
            None,
        );
        assert!(score.passed, "{score:?}");
    }

    #[test]
    fn dataset_loader_accepts_arrays() {
        let tmp = tempfile::tempdir().unwrap();
        let cases_dir = tmp.path().join("cases");
        fs::create_dir(&cases_dir).unwrap();
        fs::write(
            cases_dir.join("cases.json"),
            serde_json::to_string(&vec![exact_case()]).unwrap(),
        )
        .unwrap();
        let loaded = load_tool_call_eval_dataset(tmp.path()).unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].id, "exact");
    }
}
