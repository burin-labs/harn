use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::cli::PersonaCompilePromptArgs;
use crate::dispatch;
use crate::env_guard::ScopedEnvVar;

const PERSONA_BLUEPRINT_ENV: &str = "HARN_PERSONA_BLUEPRINT_JSON";
const PERSONA_COMPILE_PROMPT_ENV: &str = "HARN_PERSONA_COMPILE_PROMPT_JSON";

// Embedded scripts receive input through scoped process environment. Serialize
// both persona compiler bridges so in-process callers cannot cross wires.
static PERSONA_COMPILER_DISPATCH_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

#[derive(Serialize)]
struct PersonaPromptRequest<'a> {
    prompt: &'a str,
    name: Option<&'a str>,
    provider: Option<&'a str>,
    model: Option<&'a str>,
    max_tokens: Option<u32>,
}

pub(super) struct CompiledPersonaPrompt {
    pub(super) receipt: PersonaPromptCompileReceipt,
    raw_json: String,
}

#[derive(Debug, Deserialize)]
pub(super) struct PersonaPromptCompileReceipt {
    #[expect(
        dead_code,
        reason = "Serde decodes the closed receipt version at this boundary."
    )]
    schema_version: PersonaPromptCompileSchemaVersion,
    ok: bool,
    prompt_digest: String,
    catalog_digest: String,
    checkpoint: PersonaPromptCheckpointReceipt,
    usage: PersonaPromptUsage,
    blueprint: Option<serde_json::Value>,
    lowering: Option<PromptCompiledPersonaLowering>,
    error: Option<PersonaPromptCompileError>,
}

#[derive(Debug, Deserialize)]
enum PersonaPromptCompileSchemaVersion {
    #[serde(rename = "harn.persona.prompt_compile.v1")]
    V1,
}

#[derive(Debug, Deserialize)]
struct PersonaPromptCheckpointReceipt {
    status: PersonaPromptCheckpointStatus,
    attempts: u64,
    repaired: bool,
    provider: String,
    model: String,
}

#[derive(Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum PersonaPromptCheckpointStatus {
    NotAttempted,
    Accepted,
    SchemaRejected,
    ValidatorRejected,
}

impl PersonaPromptCheckpointStatus {
    fn as_str(&self) -> &'static str {
        match self {
            Self::NotAttempted => "not_attempted",
            Self::Accepted => "accepted",
            Self::SchemaRejected => "schema_rejected",
            Self::ValidatorRejected => "validator_rejected",
        }
    }
}

#[derive(Debug, Deserialize)]
struct PersonaPromptUsage {
    input_tokens: u64,
    output_tokens: u64,
    total_tokens: u64,
    realized_cost_usd: Option<f64>,
}

#[derive(Debug, Deserialize)]
struct PersonaPromptCompileError {
    code: String,
    message: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct MaterializeCompileResponse {
    ok: bool,
    lowering: Option<PromptCompiledPersonaLowering>,
    error: Option<PersonaBlueprintCompileError>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PersonaBlueprintCompileError {
    code: String,
    message: String,
}

impl PersonaBlueprintCompileError {
    fn bridge(code: &str, message: impl Into<String>) -> Self {
        Self {
            code: code.to_string(),
            message: message.into(),
        }
    }

    fn coded_message(self) -> String {
        format!("{}: {}", self.code, self.message)
    }
}

#[derive(Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(super) struct PromptCompiledPersonaLowering {
    profile: PromptCompiledProfile,
    pub(super) template: String,
    pub(super) persona: PromptCompiledPersona,
    policy: PromptCompiledPolicy,
    pub(super) triggers: Vec<PromptCompiledTrigger>,
}

#[derive(Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum PromptCompiledProfile {
    PromptCompiledV1,
}

#[derive(Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(super) struct PromptCompiledPersona {
    pub(super) name: String,
    pub(super) description: String,
    pub(super) goal: String,
}

#[derive(Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct PromptCompiledPolicy {
    autonomy_tier: SuggestAutonomyTier,
    receipt_policy: RequiredReceiptPolicy,
}

#[derive(Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum SuggestAutonomyTier {
    Suggest,
}

#[derive(Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum RequiredReceiptPolicy {
    Required,
}

#[derive(Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(super) struct PromptCompiledTrigger {
    pub(super) id: String,
    pub(super) kind: String,
    pub(super) provider: String,
    pub(super) events: Vec<String>,
    pub(super) secrets: BTreeMap<String, String>,
    pub(super) schedule: Option<String>,
    pub(super) timezone: Option<String>,
    pub(super) handler: String,
}

impl PersonaPromptCompileReceipt {
    pub(super) fn into_lowering(self) -> Result<PromptCompiledPersonaLowering, String> {
        if !self.ok {
            return Err(self
                .error
                .map(|error| format!("{}: {}", error.code, error.message))
                .unwrap_or_else(|| "persona prompt compilation failed".to_string()));
        }
        let lowering = self
            .lowering
            .ok_or_else(|| "persona prompt compiler returned no lowering".to_string())?;
        Ok(lowering)
    }

    fn into_reviewed_parts(
        self,
    ) -> Result<(serde_json::Value, PromptCompiledPersonaLowering), String> {
        if !self.ok {
            return Err(self
                .error
                .map(|error| format!("{}: {}", error.code, error.message))
                .unwrap_or_else(|| "persona prompt compilation failed".to_string()));
        }
        if self.checkpoint.status != PersonaPromptCheckpointStatus::Accepted {
            return Err(format!(
                "persona compile receipt checkpoint must be accepted, got {}",
                self.checkpoint.status.as_str()
            ));
        }
        let blueprint = self
            .blueprint
            .ok_or_else(|| "persona compile receipt has no blueprint".to_string())?;
        let lowering = self
            .lowering
            .ok_or_else(|| "persona compile receipt has no reviewed lowering".to_string())?;
        Ok((blueprint, lowering))
    }
}

async fn run_compiler_script(
    script: &str,
    env_name: &'static str,
    payload: &str,
    failure: &str,
) -> Result<String, String> {
    let _dispatch = PERSONA_COMPILER_DISPATCH_LOCK.lock().await;
    let _payload = ScopedEnvVar::set(env_name, payload);
    let outcome = dispatch::run_embedded_script(script, Vec::new(), true).await;
    if outcome.exit_code != 0 {
        let detail = outcome.stderr.trim();
        return Err(if detail.is_empty() {
            failure.to_string()
        } else {
            format!("{failure}: {detail}")
        });
    }
    Ok(outcome.stdout.trim().to_string())
}

pub(super) async fn compile_blueprint_path(
    blueprint_path: &Path,
) -> Result<PromptCompiledPersonaLowering, String> {
    let bytes = fs::read(blueprint_path)
        .map_err(|error| format!("failed to read {}: {error}", blueprint_path.display()))?;
    let blueprint = String::from_utf8(bytes).map_err(|error| {
        format!(
            "persona blueprint {} must be UTF-8 JSON: {error}",
            blueprint_path.display()
        )
    })?;
    compile_blueprint_json(&blueprint)
        .await
        .map_err(|error| error.message)
}

async fn compile_blueprint_json(
    blueprint: &str,
) -> Result<PromptCompiledPersonaLowering, PersonaBlueprintCompileError> {
    let output = run_compiler_script(
        "personas/materialize",
        PERSONA_BLUEPRINT_ENV,
        blueprint,
        "persona blueprint compiler failed",
    )
    .await
    .map_err(|error| PersonaBlueprintCompileError::bridge("compiler_bridge_failed", error))?;
    let response =
        serde_json::from_str::<MaterializeCompileResponse>(&output).map_err(|error| {
            PersonaBlueprintCompileError::bridge(
                "invalid_compiler_response",
                format!("persona blueprint compiler returned invalid JSON: {error}"),
            )
        })?;
    if !response.ok {
        return Err(response.error.unwrap_or_else(|| {
            PersonaBlueprintCompileError::bridge(
                "blueprint_invalid",
                "persona blueprint failed Harn validation",
            )
        }));
    }
    let lowering = response.lowering.ok_or_else(|| {
        PersonaBlueprintCompileError::bridge(
            "missing_compiler_lowering",
            "persona blueprint compiler returned no lowering",
        )
    })?;
    Ok(lowering)
}

pub(super) async fn compile_reviewed_receipt_path(
    receipt_path: &Path,
) -> Result<PromptCompiledPersonaLowering, String> {
    let bytes = fs::read(receipt_path)
        .map_err(|error| format!("failed to read {}: {error}", receipt_path.display()))?;
    let receipt_json = String::from_utf8(bytes).map_err(|error| {
        format!(
            "persona compile receipt {} must be UTF-8 JSON: {error}",
            receipt_path.display()
        )
    })?;
    let receipt = serde_json::from_str::<PersonaPromptCompileReceipt>(&receipt_json)
        .map_err(|error| format!("persona compile receipt is invalid: {error}"))?;
    let (blueprint, reviewed_lowering) = receipt.into_reviewed_parts()?;
    let blueprint_json = serde_json::to_string(&blueprint)
        .map_err(|error| format!("failed to encode receipt blueprint: {error}"))?;
    let current_lowering = compile_blueprint_json(&blueprint_json)
        .await
        .map_err(PersonaBlueprintCompileError::coded_message)?;
    if current_lowering != reviewed_lowering {
        return Err(
            "persona compile receipt lowering no longer matches current Harn lowering; compile and review a fresh receipt"
                .to_string(),
        );
    }
    Ok(current_lowering)
}

pub(super) async fn compile_prompt(
    prompt: &str,
    name: Option<&str>,
    provider: Option<&str>,
    model: Option<&str>,
    max_tokens: Option<u32>,
) -> Result<CompiledPersonaPrompt, String> {
    let payload = serde_json::to_string(&PersonaPromptRequest {
        prompt,
        name,
        provider,
        model,
        max_tokens,
    })
    .map_err(|error| format!("failed to encode persona prompt request: {error}"))?;
    let raw_json = run_compiler_script(
        "personas/compile_prompt",
        PERSONA_COMPILE_PROMPT_ENV,
        &payload,
        "persona prompt compiler failed",
    )
    .await?;
    let receipt = serde_json::from_str::<PersonaPromptCompileReceipt>(&raw_json)
        .map_err(|error| format!("persona prompt compiler returned invalid JSON: {error}"))?;
    Ok(CompiledPersonaPrompt { receipt, raw_json })
}

pub(crate) async fn run_compile_prompt(args: &PersonaCompilePromptArgs) -> Result<(), String> {
    let compiled = compile_prompt(
        &args.prompt,
        args.name.as_deref(),
        args.provider.as_deref(),
        args.model.as_deref(),
        Some(args.max_tokens),
    )
    .await?;
    if args.json {
        println!("{}", compiled.raw_json);
    } else {
        println!("status: {}", compiled.receipt.checkpoint.status.as_str());
        println!("attempts: {}", compiled.receipt.checkpoint.attempts);
        println!("repaired: {}", compiled.receipt.checkpoint.repaired);
        println!("prompt_digest: {}", compiled.receipt.prompt_digest);
        println!("catalog_digest: {}", compiled.receipt.catalog_digest);
        println!("provider: {}", compiled.receipt.checkpoint.provider);
        println!("model: {}", compiled.receipt.checkpoint.model);
        println!(
            "usage: input={} output={} total={} cost_usd={}",
            compiled.receipt.usage.input_tokens,
            compiled.receipt.usage.output_tokens,
            compiled.receipt.usage.total_tokens,
            compiled
                .receipt
                .usage
                .realized_cost_usd
                .map_or_else(|| "unknown".to_string(), |cost| cost.to_string())
        );
    }
    if compiled.receipt.ok {
        Ok(())
    } else {
        Err(compiled
            .receipt
            .error
            .map(|error| format!("{}: {}", error.code, error.message))
            .unwrap_or_else(|| "persona prompt compilation failed".to_string()))
    }
}
