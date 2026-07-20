use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use harn_vm::llm::capabilities::{self, Capabilities, ProviderCapabilityMatrixRow};
use harn_vm::provider_catalog::{
    self, CatalogModel, CatalogProvider, DeprecationStatus, ProviderCatalogArtifact,
    ProviderClassification,
};
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;

use crate::cli::ProvidersSupportArgs;
use crate::format::escape_md;

pub(crate) const PROVIDER_SUPPORT_SCHEMA_VERSION: u32 = 1;
const DEFAULT_NOTES_PATH: &str = "crates/harn-cli/data/provider_support_notes.toml";
const EMBEDDED_NOTES_TOML: &str = include_str!("../../data/provider_support_notes.toml");

pub(crate) fn run(args: &ProvidersSupportArgs) -> Result<(), String> {
    let report = build_report(&args.notes, &args.empirical)?;
    let markdown = render_markdown(&report);
    let json = render_json(&report)?;

    if args.check {
        check_file(&args.output, &markdown)?;
        check_file(&args.json_output, &json)?;
        if !args.stdout && !args.json {
            println!("provider support artifacts are up to date");
        }
        return Ok(());
    }

    if args.json {
        print!("{json}");
        return Ok(());
    }
    if args.stdout {
        print!("{markdown}");
        return Ok(());
    }

    write_file(&args.output, &markdown)?;
    write_file(&args.json_output, &json)?;
    println!("wrote {}", args.output.display());
    println!("wrote {}", args.json_output.display());
    Ok(())
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct ProviderSupportReport {
    pub schema_version: u32,
    pub generated_by: String,
    pub sources: ProviderSupportSources,
    pub providers: Vec<ProviderSupportEntry>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct ProviderSupportSources {
    pub provider_catalog_schema_version: u32,
    pub capability_matrix: String,
    pub notes: String,
    pub empirical: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct ProviderSupportEntry {
    pub id: String,
    pub display_name: String,
    pub catalog_provider: String,
    pub endpoint_style: String,
    pub classification: String,
    pub auth_env: Vec<String>,
    pub recommended: RecommendedRoute,
    pub capabilities: SupportCapabilities,
    pub empirical: EmpiricalSupportSummary,
    pub notes: Vec<String>,
    pub caveats: Vec<String>,
    pub mcp_notes: Vec<String>,
    pub local_setup_notes: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct RecommendedRoute {
    pub selector: String,
    pub provider: String,
    pub model: String,
    pub display_name: Option<String>,
    pub tool_format: String,
    pub harn_options: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct SupportCapabilities {
    pub native_tools: bool,
    pub text_tools: bool,
    pub preferred_tool_format: String,
    pub tool_mode_parity: String,
    pub structured_output_transport: String,
    pub structured_output_mode: String,
    pub streaming: bool,
    pub reasoning_knobs: Vec<String>,
    pub prompt_or_context_cache: bool,
    pub batch_api: bool,
    pub batch_discount_percent: Option<u32>,
    pub serving_tiers: Vec<SupportServingTier>,
    pub usage_accounting_confidence: String,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct SupportServingTier {
    pub id: String,
    pub mode: String,
    pub economics: String,
    pub request_param: Option<String>,
    pub request_value: Option<String>,
    pub discount_percent: Option<u32>,
    pub cost_multiplier: Option<f64>,
    pub status: Option<String>,
    pub latency: Option<String>,
    pub reliability: Option<String>,
    pub suitable_workloads: Vec<String>,
    pub unsuitable_workloads: Vec<String>,
    pub note: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub(crate) struct EmpiricalSupportSummary {
    pub status: String,
    pub sources: Vec<String>,
    pub total_runs: usize,
    pub passed_runs: usize,
    pub failed_runs: usize,
    pub skipped_runs: usize,
    pub best_tool_format: Option<String>,
    pub native_text_parity: Option<String>,
    pub evidence: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Default)]
struct SupportNotesFile {
    #[serde(default)]
    entry: Vec<SupportNoteEntry>,
}

#[derive(Debug, Clone, Deserialize, Default)]
struct SupportNoteEntry {
    id: String,
    #[serde(default)]
    catalog_provider: Option<String>,
    #[serde(default)]
    display_name: Option<String>,
    #[serde(default)]
    endpoint_style: Option<String>,
    #[serde(default)]
    recommended_model: Option<String>,
    #[serde(default)]
    recommended_selector: Option<String>,
    #[serde(default)]
    recommended_tool_format: Option<String>,
    #[serde(default)]
    recommended_options: Vec<String>,
    #[serde(default)]
    usage_confidence: Option<String>,
    #[serde(default)]
    notes: Vec<String>,
    #[serde(default)]
    caveats: Vec<String>,
    #[serde(default)]
    mcp_notes: Vec<String>,
    #[serde(default)]
    local_setup_notes: Vec<String>,
}

#[derive(Debug, Default)]
struct EmpiricalIndex {
    sources: BTreeSet<String>,
    by_provider: BTreeMap<String, EmpiricalBucket>,
    by_model: BTreeMap<(String, String), EmpiricalBucket>,
}

#[derive(Debug, Default, Clone)]
struct EmpiricalBucket {
    sources: BTreeSet<String>,
    total_runs: usize,
    passed_runs: usize,
    skipped_runs: usize,
    by_tool_format: BTreeMap<String, ToolFormatStats>,
    parity: BTreeSet<String>,
    evidence: Vec<String>,
}

#[derive(Debug, Default, Clone)]
struct ToolFormatStats {
    total: usize,
    passed: usize,
}

pub(crate) fn build_report(
    notes_path: &Path,
    empirical_paths: &[PathBuf],
) -> Result<ProviderSupportReport, String> {
    let catalog = provider_catalog::artifact();
    let notes = load_notes(notes_path)?;
    let empirical = load_empirical(empirical_paths)?;
    Ok(build_report_from_parts(
        catalog,
        notes_source_label(notes_path),
        notes,
        empirical,
    ))
}

fn build_report_from_parts(
    catalog: ProviderCatalogArtifact,
    notes_source: String,
    notes: SupportNotesFile,
    empirical: EmpiricalIndex,
) -> ProviderSupportReport {
    let providers_by_id = catalog
        .providers
        .iter()
        .map(|provider| (provider.id.clone(), provider))
        .collect::<BTreeMap<_, _>>();
    let models_by_key = catalog
        .models
        .iter()
        .map(|model| ((model.provider.clone(), model.id.clone()), model))
        .collect::<BTreeMap<_, _>>();
    let mut models_by_provider: BTreeMap<String, Vec<&CatalogModel>> = BTreeMap::new();
    for model in &catalog.models {
        models_by_provider
            .entry(model.provider.clone())
            .or_default()
            .push(model);
    }
    let mut capability_rows_by_provider: BTreeMap<String, Vec<ProviderCapabilityMatrixRow>> =
        BTreeMap::new();
    for row in capabilities::matrix_rows() {
        capability_rows_by_provider
            .entry(row.provider.clone())
            .or_default()
            .push(row);
    }
    let note_by_id = notes
        .entry
        .iter()
        .map(|entry| (entry.id.clone(), entry))
        .collect::<BTreeMap<_, _>>();
    let mut ids = notes
        .entry
        .iter()
        .map(|entry| entry.id.clone())
        .collect::<Vec<_>>();
    for provider in &catalog.providers {
        if !note_by_id.contains_key(&provider.id) {
            ids.push(provider.id.clone());
        }
    }
    ids.sort();
    ids.dedup();

    let providers = ids
        .into_iter()
        .map(|id| {
            let note = note_by_id.get(&id).copied();
            build_entry(
                &id,
                note,
                &providers_by_id,
                &models_by_provider,
                &models_by_key,
                &capability_rows_by_provider,
                &catalog.qc_defaults,
                &empirical,
            )
        })
        .collect();

    ProviderSupportReport {
        schema_version: PROVIDER_SUPPORT_SCHEMA_VERSION,
        generated_by: "harn provider catalog support".to_string(),
        sources: ProviderSupportSources {
            provider_catalog_schema_version: catalog.schema_version,
            capability_matrix: "crates/harn-vm/src/llm/capabilities.toml".to_string(),
            notes: notes_source,
            empirical: empirical.sources.into_iter().collect(),
        },
        providers,
    }
}

fn build_entry(
    id: &str,
    note: Option<&SupportNoteEntry>,
    providers_by_id: &BTreeMap<String, &CatalogProvider>,
    models_by_provider: &BTreeMap<String, Vec<&CatalogModel>>,
    models_by_key: &BTreeMap<(String, String), &CatalogModel>,
    capability_rows_by_provider: &BTreeMap<String, Vec<ProviderCapabilityMatrixRow>>,
    qc_defaults: &BTreeMap<String, String>,
    empirical: &EmpiricalIndex,
) -> ProviderSupportEntry {
    let catalog_provider = note
        .and_then(|entry| entry.catalog_provider.as_deref())
        .unwrap_or(id);
    let provider = providers_by_id.get(catalog_provider).copied();
    let model_id = recommended_model_id(
        note,
        catalog_provider,
        models_by_provider,
        capability_rows_by_provider,
        qc_defaults,
    )
    .unwrap_or_else(|| "*".to_string());
    let model = models_by_key
        .get(&(catalog_provider.to_string(), model_id.clone()))
        .copied();
    let provider_models = models_by_provider
        .get(catalog_provider)
        .map(Vec::as_slice)
        .unwrap_or_default();
    let caps = if model_id == "*" {
        Capabilities::default()
    } else {
        capabilities::lookup(catalog_provider, &model_id)
    };
    let recommended_tool_format = note
        .and_then(|entry| entry.recommended_tool_format.as_deref())
        .map(str::to_string)
        .or_else(|| model.and_then(|model| model.tool_support.preferred_format.clone()))
        .or_else(|| caps.preferred_tool_format.clone())
        .unwrap_or_else(|| {
            if caps.native_tools {
                "native".to_string()
            } else {
                "text".to_string()
            }
        });
    let selector = note
        .and_then(|entry| entry.recommended_selector.clone())
        .unwrap_or_else(|| selector_for(catalog_provider, &model_id));
    let empirical_summary = empirical_summary_for(empirical, catalog_provider, &model_id);
    let display_name = note
        .and_then(|entry| entry.display_name.clone())
        .or_else(|| provider.map(|provider| provider.display_name.clone()))
        .unwrap_or_else(|| id.to_string());
    let batch_api = harn_vm::llm_config::effective_batch_api_supported(catalog_provider, &caps);

    ProviderSupportEntry {
        id: id.to_string(),
        display_name,
        catalog_provider: catalog_provider.to_string(),
        endpoint_style: note
            .and_then(|entry| entry.endpoint_style.clone())
            .unwrap_or_else(|| endpoint_style(provider, catalog_provider, &caps)),
        classification: provider.map(classification).unwrap_or("custom").to_string(),
        auth_env: provider
            .map(|provider| provider.auth.env.clone())
            .unwrap_or_default(),
        recommended: RecommendedRoute {
            selector,
            provider: catalog_provider.to_string(),
            model: model_id,
            display_name: model.map(|model| model.name.clone()),
            tool_format: recommended_tool_format,
            harn_options: note
                .map(|entry| entry.recommended_options.clone())
                .unwrap_or_else(|| default_options(catalog_provider, model, &caps)),
        },
        capabilities: SupportCapabilities {
            native_tools: caps.native_tools,
            text_tools: caps.text_tool_wire_format_supported,
            preferred_tool_format: caps
                .preferred_tool_format
                .clone()
                .unwrap_or_else(|| "auto".to_string()),
            tool_mode_parity: caps
                .tool_mode_parity
                .clone()
                .unwrap_or_else(|| "unknown".to_string()),
            structured_output_transport: caps
                .structured_output
                .clone()
                .unwrap_or_else(|| "none".to_string()),
            structured_output_mode: caps.structured_output_mode.clone(),
            streaming: true,
            reasoning_knobs: reasoning_knobs(&caps),
            prompt_or_context_cache: caps.prompt_caching
                || model.is_some_and(model_has_cache_pricing),
            batch_api,
            batch_discount_percent: batch_api.then_some(caps.batch_discount_percent).flatten(),
            serving_tiers: support_serving_tiers(provider_models),
            usage_accounting_confidence: note
                .and_then(|entry| entry.usage_confidence.clone())
                .unwrap_or_else(|| usage_confidence(provider, model)),
        },
        empirical: empirical_summary,
        notes: note.map(|entry| entry.notes.clone()).unwrap_or_default(),
        caveats: merged_caveats(note, &caps),
        mcp_notes: note
            .map(|entry| entry.mcp_notes.clone())
            .unwrap_or_default(),
        local_setup_notes: note
            .map(|entry| entry.local_setup_notes.clone())
            .unwrap_or_default(),
    }
}

fn recommended_model_id(
    note: Option<&SupportNoteEntry>,
    provider: &str,
    models_by_provider: &BTreeMap<String, Vec<&CatalogModel>>,
    capability_rows_by_provider: &BTreeMap<String, Vec<ProviderCapabilityMatrixRow>>,
    qc_defaults: &BTreeMap<String, String>,
) -> Option<String> {
    note.and_then(|entry| entry.recommended_model.clone())
        .or_else(|| qc_defaults.get(provider).cloned())
        .or_else(|| {
            models_by_provider
                .get(provider)
                .and_then(|models| {
                    models
                        .iter()
                        .filter(|model| {
                            matches!(&model.deprecation.status, DeprecationStatus::Active)
                        })
                        .min_by(|a, b| {
                            model_price_rank(a)
                                .cmp(&model_price_rank(b))
                                .then_with(|| a.id.cmp(&b.id))
                        })
                })
                .map(|model| model.id.clone())
        })
        .or_else(|| {
            capability_rows_by_provider
                .get(provider)
                .and_then(|rows| rows.iter().find(|row| row.tools))
                .map(|row| row.model.clone())
        })
}

fn model_price_rank(model: &CatalogModel) -> u64 {
    model
        .pricing
        .as_ref()
        .map(|pricing| ((pricing.input_per_mtok + pricing.output_per_mtok) * 1000.0) as u64)
        .unwrap_or(u64::MAX)
}

fn selector_for(provider: &str, model: &str) -> String {
    if model == "*" {
        return provider.to_string();
    }
    match provider {
        "ollama" => format!("ollama:{model}"),
        _ => format!("{provider}:{model}"),
    }
}

fn endpoint_style(
    provider: Option<&CatalogProvider>,
    provider_id: &str,
    caps: &Capabilities,
) -> String {
    if let Some(provider) = provider {
        if provider.endpoint.chat_endpoint.contains("generateContent") {
            return "Gemini generateContent".to_string();
        }
        if provider.endpoint.chat_endpoint.contains("/messages") {
            return "Anthropic Messages API".to_string();
        }
        if provider.endpoint.chat_endpoint.contains("converse") {
            return "AWS Bedrock Converse".to_string();
        }
        if provider
            .endpoint
            .chat_endpoint
            .contains("/chat/completions")
        {
            return "OpenAI-compatible chat completions".to_string();
        }
        if provider_id == "ollama" {
            return "Ollama native chat API".to_string();
        }
    }
    match caps.message_wire_format.as_str() {
        "anthropic" => "Anthropic Messages API".to_string(),
        "gemini" => "Gemini generateContent".to_string(),
        "ollama" => "Ollama native chat API".to_string(),
        _ => "OpenAI-compatible chat completions".to_string(),
    }
}

fn classification(provider: &CatalogProvider) -> &'static str {
    match &provider.classification {
        ProviderClassification::Hosted => "hosted",
        ProviderClassification::Local => "local",
    }
}

fn default_options(
    provider: &str,
    model: Option<&CatalogModel>,
    caps: &Capabilities,
) -> Vec<String> {
    let mut options = vec![format!("provider = \"{provider}\"")];
    if let Some(model) = model {
        options.push(format!("model = \"{}\"", model.id));
    }
    if let Some(tool_format) = caps.preferred_tool_format.as_ref() {
        options.push(format!("tool_format = \"{tool_format}\""));
    }
    if caps.structured_output_mode != "none" {
        options.push(format!(
            "structured_output_mode = \"{}\"",
            caps.structured_output_mode
        ));
    }
    options
}

fn reasoning_knobs(caps: &Capabilities) -> Vec<String> {
    let mut knobs = caps.thinking_modes.clone();
    if caps.reasoning_effort_supported {
        knobs.push("reasoning_effort".to_string());
    }
    if caps.reasoning_none_supported {
        knobs.push("reasoning_none".to_string());
    }
    if let Some(directive) = caps.thinking_disable_directive.as_ref() {
        knobs.push(format!("disable_directive:{directive}"));
    }
    knobs.sort();
    knobs.dedup();
    knobs
}

fn model_has_cache_pricing(model: &CatalogModel) -> bool {
    model.pricing.as_ref().is_some_and(|pricing| {
        pricing.cache_read_per_mtok.is_some() || pricing.cache_write_per_mtok.is_some()
    })
}

fn usage_confidence(provider: Option<&CatalogProvider>, model: Option<&CatalogModel>) -> String {
    if model.is_some_and(|model| model.pricing.is_some()) {
        "high".to_string()
    } else if provider.is_some_and(|provider| classification(provider) == "local") {
        "local_zero_cost".to_string()
    } else if provider.is_some() {
        "provider_default".to_string()
    } else {
        "unknown".to_string()
    }
}

fn merged_caveats(note: Option<&SupportNoteEntry>, caps: &Capabilities) -> Vec<String> {
    let mut caveats = note.map(|entry| entry.caveats.clone()).unwrap_or_default();
    if let Some(parity_notes) = caps.tool_mode_parity_notes.as_ref() {
        caveats.push(parity_notes.clone());
    }
    caveats
}

fn empirical_summary_for(
    empirical: &EmpiricalIndex,
    provider: &str,
    model: &str,
) -> EmpiricalSupportSummary {
    if let Some(bucket) = empirical
        .by_model
        .get(&(provider.to_string(), model.to_string()))
    {
        return bucket.to_summary();
    }

    if model.contains('*') {
        let mut bucket = EmpiricalBucket::default();
        for ((run_provider, run_model), run_bucket) in &empirical.by_model {
            if run_provider == provider && model_pattern_matches(model, run_model) {
                bucket.merge(run_bucket);
            }
        }
        if bucket.has_observations() {
            return bucket.to_summary();
        }
    }

    if model == "*" {
        if let Some(bucket) = empirical.by_provider.get(provider) {
            return bucket.to_summary();
        }
    }

    EmpiricalSupportSummary {
        status: "not_recorded".to_string(),
        ..EmpiricalSupportSummary::default()
    }
}

impl EmpiricalBucket {
    fn has_observations(&self) -> bool {
        self.total_runs > 0 || !self.parity.is_empty()
    }

    fn merge(&mut self, other: &Self) {
        self.sources.extend(other.sources.iter().cloned());
        self.total_runs += other.total_runs;
        self.passed_runs += other.passed_runs;
        self.skipped_runs += other.skipped_runs;
        for (format, stats) in &other.by_tool_format {
            let merged = self.by_tool_format.entry(format.clone()).or_default();
            merged.total += stats.total;
            merged.passed += stats.passed;
        }
        self.parity.extend(other.parity.iter().cloned());
        for item in &other.evidence {
            if self.evidence.len() >= 8 {
                break;
            }
            self.evidence.push(item.clone());
        }
    }

    fn add_run(&mut self, source: &str, run: &JsonValue) {
        self.sources.insert(source.to_string());
        self.total_runs += 1;
        let passed = run
            .get("passed")
            .and_then(JsonValue::as_bool)
            .unwrap_or(false);
        let skipped = run
            .get("skipped")
            .and_then(JsonValue::as_bool)
            .unwrap_or(false);
        if passed {
            self.passed_runs += 1;
        }
        if skipped {
            self.skipped_runs += 1;
        }
        let tool_format = run
            .get("tool_format")
            .and_then(JsonValue::as_str)
            .unwrap_or("unknown")
            .to_string();
        let stats = self.by_tool_format.entry(tool_format.clone()).or_default();
        stats.total += 1;
        if passed {
            stats.passed += 1;
        }
        if self.evidence.len() < 8 {
            let run_id = run
                .get("run_id")
                .and_then(JsonValue::as_str)
                .unwrap_or("unknown-run");
            let status = run
                .get("status")
                .and_then(JsonValue::as_str)
                .unwrap_or("unknown");
            self.evidence
                .push(format!("{run_id} {tool_format}: {status}"));
        }
    }

    fn add_parity(&mut self, parity: &str) {
        self.parity.insert(parity.to_string());
    }

    fn to_summary(&self) -> EmpiricalSupportSummary {
        let failed_runs = self
            .total_runs
            .saturating_sub(self.passed_runs)
            .saturating_sub(self.skipped_runs);
        let status = if self.total_runs == 0 {
            "not_recorded"
        } else if self.passed_runs > 0 {
            "observed_pass"
        } else if self.skipped_runs == self.total_runs {
            "skipped"
        } else {
            "observed_failure"
        };
        EmpiricalSupportSummary {
            status: status.to_string(),
            sources: self.sources.iter().cloned().collect(),
            total_runs: self.total_runs,
            passed_runs: self.passed_runs,
            failed_runs,
            skipped_runs: self.skipped_runs,
            best_tool_format: self.best_tool_format(),
            native_text_parity: self.parity_summary(),
            evidence: self.evidence.clone(),
        }
    }

    fn best_tool_format(&self) -> Option<String> {
        self.by_tool_format
            .iter()
            .max_by(|(format_a, stats_a), (format_b, stats_b)| {
                stats_a
                    .passed
                    .cmp(&stats_b.passed)
                    .then_with(|| stats_a.total.cmp(&stats_b.total))
                    .then_with(|| format_b.cmp(format_a))
            })
            .map(|(format, _)| format.clone())
    }

    fn parity_summary(&self) -> Option<String> {
        if self.parity.is_empty() {
            None
        } else if self.parity.iter().any(|value| value == "diverged") {
            Some("diverged".to_string())
        } else if self.parity.iter().any(|value| value == "interchangeable") {
            Some("interchangeable".to_string())
        } else {
            Some(self.parity.iter().cloned().collect::<Vec<_>>().join(","))
        }
    }
}

fn load_empirical(paths: &[PathBuf]) -> Result<EmpiricalIndex, String> {
    let mut index = EmpiricalIndex::default();
    for path in paths {
        let source = source_label(path);
        let value = read_json(path)?;
        let runs = value
            .get("runs")
            .and_then(JsonValue::as_array)
            .ok_or_else(|| format!("{} is not a coding-agent summary JSON", path.display()))?;
        index.sources.insert(source.clone());
        for run in runs {
            let Some((provider, model)) = run_provider_model(run) else {
                continue;
            };
            index
                .by_provider
                .entry(provider.clone())
                .or_default()
                .add_run(&source, run);
            index
                .by_model
                .entry((provider, model))
                .or_default()
                .add_run(&source, run);
        }
        if let Some(comparisons) = value.get("comparisons").and_then(JsonValue::as_array) {
            for comparison in comparisons {
                let Some((provider, model)) = comparison_provider_model(comparison) else {
                    continue;
                };
                let parity = if comparison
                    .get("equivalent")
                    .and_then(JsonValue::as_bool)
                    .unwrap_or(false)
                {
                    "interchangeable"
                } else {
                    "diverged"
                };
                index
                    .by_provider
                    .entry(provider.clone())
                    .or_default()
                    .add_parity(parity);
                index
                    .by_model
                    .entry((provider, model))
                    .or_default()
                    .add_parity(parity);
            }
        }
    }
    Ok(index)
}

fn run_provider_model(run: &JsonValue) -> Option<(String, String)> {
    let selector = run.get("selector")?;
    Some((
        selector.get("provider")?.as_str()?.to_string(),
        selector.get("model")?.as_str()?.to_string(),
    ))
}

fn comparison_provider_model(comparison: &JsonValue) -> Option<(String, String)> {
    let selector = comparison.get("selector")?;
    Some((
        selector.get("provider")?.as_str()?.to_string(),
        selector.get("model")?.as_str()?.to_string(),
    ))
}

/// Match a configured model pattern against a model id.
///
/// Model ids are flat names, so this uses [`harn_glob::match_name`] (full glob
/// syntax, `/` not special). Matching stays case-insensitive, as it has always
/// been for this surface: patterns are hand-written in config and model ids are
/// conventionally — but not reliably — lowercase.
fn model_pattern_matches(pattern: &str, model: &str) -> bool {
    harn_glob::match_name(&pattern.to_lowercase(), &model.to_lowercase())
}

fn load_notes(path: &Path) -> Result<SupportNotesFile, String> {
    let raw = if path.exists() {
        fs::read_to_string(path)
            .map_err(|error| format!("failed to read {}: {error}", path.display()))?
    } else if path == Path::new(DEFAULT_NOTES_PATH) {
        EMBEDDED_NOTES_TOML.to_string()
    } else {
        return Err(format!(
            "provider support notes not found: {}",
            path.display()
        ));
    };
    toml::from_str(&raw).map_err(|error| format!("failed to parse {}: {error}", path.display()))
}

fn read_json(path: &Path) -> Result<JsonValue, String> {
    let raw = fs::read_to_string(path)
        .map_err(|error| format!("failed to read {}: {error}", path.display()))?;
    serde_json::from_str(&raw)
        .map_err(|error| format!("failed to parse {}: {error}", path.display()))
}

fn source_label(path: &Path) -> String {
    path.file_name()
        .and_then(|name| name.to_str())
        .map(str::to_string)
        .unwrap_or_else(|| path.display().to_string())
}

fn notes_source_label(path: &Path) -> String {
    if path == Path::new(DEFAULT_NOTES_PATH) {
        DEFAULT_NOTES_PATH.to_string()
    } else {
        source_label(path)
    }
}

pub(crate) fn render_json(report: &ProviderSupportReport) -> Result<String, String> {
    serde_json::to_string_pretty(report)
        .map(|mut json| {
            json.push('\n');
            json
        })
        .map_err(|error| format!("failed to render provider support JSON: {error}"))
}

pub(crate) fn render_markdown(report: &ProviderSupportReport) -> String {
    let mut out = String::new();
    out.push_str("# Provider support recommendations\n\n");
    out.push_str("<!-- GENERATED by `harn provider catalog support` -- do not edit by hand. -->\n");
    out.push_str("<!-- Sources: provider catalog, capability matrix, provider support notes, optional coding-agent benchmark summaries. -->\n\n");
    out.push_str("<!-- markdownlint-disable MD013 -->\n\n");
    out.push_str("This page aggregates Harn's provider/model catalog, runtime capability rules, small curated notes, and optional `harn eval coding-agent` benchmark summaries. Regenerate with `make gen-provider-support` and verify with `make check-provider-support`.\n\n");
    if report.sources.empirical.is_empty() {
        out.push_str("No benchmark summary is baked into this checked-in page. To layer local empirical results, run `harn provider catalog support --empirical .harn-runs/coding-agent-bench/latest/summary.json`.\n\n");
    } else {
        out.push_str(&format!(
            "Empirical sources: `{}`.\n\n",
            report.sources.empirical.join("`, `")
        ));
    }
    out.push_str("| Provider | Endpoint style | Recommended selector | Tool mode | Native tools | Text tools | Structured output | Reasoning knobs | Cache | Batch | Serving tiers | Usage confidence | Empirical |\n");
    out.push_str("|---|---|---|---|---:|---:|---|---|---:|---|---|---|---|\n");
    for entry in &report.providers {
        out.push_str(&format!(
            "| `{}` | {} | `{}` | `{}` | {} | {} | `{}` / `{}` | {} | {} | {} | {} | `{}` | {} |\n",
            markdown_escape(&entry.display_name),
            markdown_escape(&entry.endpoint_style),
            markdown_escape(&entry.recommended.selector),
            entry.recommended.tool_format,
            yes_no(entry.capabilities.native_tools),
            yes_no(entry.capabilities.text_tools),
            markdown_escape(&entry.capabilities.structured_output_transport),
            markdown_escape(&entry.capabilities.structured_output_mode),
            if entry.capabilities.reasoning_knobs.is_empty() {
                "none".to_string()
            } else {
                format!(
                    "`{}`",
                    markdown_escape(&entry.capabilities.reasoning_knobs.join(","))
                )
            },
            yes_no(entry.capabilities.prompt_or_context_cache),
            batch_cell(&entry.capabilities),
            serving_tiers_cell(&entry.capabilities),
            markdown_escape(&entry.capabilities.usage_accounting_confidence),
            empirical_cell(&entry.empirical),
        ));
    }

    out.push_str("\n## Recommended options\n\n");
    for entry in &report.providers {
        if entry.notes.is_empty()
            && entry.caveats.is_empty()
            && entry.mcp_notes.is_empty()
            && entry.local_setup_notes.is_empty()
        {
            continue;
        }
        out.push_str(&format!("### {}\n\n", entry.display_name));
        out.push_str(&format!(
            "- catalog provider: `{}`\n- recommended route: `{}` (`{}`)\n- endpoint style: {}\n",
            entry.catalog_provider,
            entry.recommended.selector,
            entry.recommended.model,
            entry.endpoint_style
        ));
        if !entry.recommended.harn_options.is_empty() {
            out.push_str("- recommended Harn options:\n\n");
            out.push_str("```toml\n");
            for option in &entry.recommended.harn_options {
                out.push_str(option);
                out.push('\n');
            }
            out.push_str("```\n");
        }
        render_bullets(&mut out, "Notes", &entry.notes);
        render_bullets(&mut out, "Caveats", &entry.caveats);
        render_bullets(&mut out, "MCP notes", &entry.mcp_notes);
        render_bullets(&mut out, "Local setup", &entry.local_setup_notes);
        out.push('\n');
    }
    while out.ends_with("\n\n") {
        out.pop();
    }
    out
}

fn empirical_cell(summary: &EmpiricalSupportSummary) -> String {
    if summary.total_runs == 0 {
        return "`not_recorded`".to_string();
    }
    let best = summary.best_tool_format.as_deref().unwrap_or("unknown");
    format!(
        "`{}` {}/{} pass, best `{}`",
        summary.status, summary.passed_runs, summary.total_runs, best
    )
}

fn batch_cell(capabilities: &SupportCapabilities) -> String {
    if !capabilities.batch_api {
        return "No".to_string();
    }
    capabilities
        .batch_discount_percent
        .map(|discount| format!("Yes ({discount}%)"))
        .unwrap_or_else(|| "Yes".to_string())
}

fn support_serving_tiers(models: &[&CatalogModel]) -> Vec<SupportServingTier> {
    let mut seen = BTreeSet::new();
    let mut tiers = Vec::new();
    for model in models
        .iter()
        .filter(|model| matches!(&model.deprecation.status, DeprecationStatus::Active))
    {
        for tier in &model.serving_tiers {
            let request = tier.request.as_ref();
            let mode = serving_tier_mode(&tier.mode).to_string();
            let economics = serving_tier_economics(&tier.economics).to_string();
            let request_param = request.map(|request| request.param.clone());
            let request_value = request.map(|request| request.value.clone());
            let key = (
                tier.id.clone(),
                mode.clone(),
                economics.clone(),
                request_param.clone(),
                request_value.clone(),
            );
            if !seen.insert(key) {
                continue;
            }
            tiers.push(SupportServingTier {
                id: tier.id.clone(),
                mode,
                economics,
                request_param,
                request_value,
                discount_percent: tier.discount_percent,
                cost_multiplier: tier.cost_multiplier,
                status: tier.status.clone(),
                latency: tier.latency.clone(),
                reliability: tier.reliability.clone(),
                suitable_workloads: tier.suitable_workloads.clone(),
                unsuitable_workloads: tier.unsuitable_workloads.clone(),
                note: tier.note.clone(),
            });
        }
    }
    tiers
}

fn serving_tier_mode(mode: &harn_vm::llm_config::ServingTierMode) -> &'static str {
    match mode {
        harn_vm::llm_config::ServingTierMode::Synchronous => "synchronous",
    }
}

fn serving_tier_economics(economics: &harn_vm::llm_config::ServingTierEconomics) -> &'static str {
    match economics {
        harn_vm::llm_config::ServingTierEconomics::Discounted => "discounted",
        harn_vm::llm_config::ServingTierEconomics::Standard => "standard",
        harn_vm::llm_config::ServingTierEconomics::Premium => "premium",
    }
}

fn serving_tiers_cell(capabilities: &SupportCapabilities) -> String {
    if capabilities.serving_tiers.is_empty() {
        return "none".to_string();
    }
    capabilities
        .serving_tiers
        .iter()
        .map(|tier| {
            let economics = markdown_escape(&tier.economics);
            format!("`{}:{}`", markdown_escape(&tier.id), economics)
        })
        .collect::<Vec<_>>()
        .join(", ")
}

fn render_bullets(out: &mut String, title: &str, items: &[String]) {
    if items.is_empty() {
        return;
    }
    out.push_str(&format!("\n{title}:\n\n"));
    for item in items {
        out.push_str(&format!("- {item}\n"));
    }
}

fn yes_no(value: bool) -> &'static str {
    if value {
        "yes"
    } else {
        "no"
    }
}

fn markdown_escape(value: &str) -> String {
    // Pipes would end the table column; newlines would break the row. Share
    // the pipe escaping with the other report commands and collapse newlines.
    escape_md(value).replace('\n', " ")
}

fn write_file(path: &Path, body: &str) -> Result<(), String> {
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        fs::create_dir_all(parent)
            .map_err(|error| format!("failed to create {}: {error}", parent.display()))?;
    }
    fs::write(path, body).map_err(|error| format!("failed to write {}: {error}", path.display()))
}

fn check_file(path: &Path, expected: &str) -> Result<(), String> {
    match fs::read_to_string(path) {
        Ok(existing) if existing == expected => Ok(()),
        Ok(_) | Err(_) => Err(format!(
            "provider support artifact is stale or missing: {}",
            path.display()
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_report_includes_core_recommendations() {
        let report = build_report(Path::new(DEFAULT_NOTES_PATH), &[]).expect("report");
        for id in [
            "anthropic",
            "openai",
            "gemini",
            "mistral",
            "ollama",
            "local",
        ] {
            assert!(
                report.providers.iter().any(|entry| entry.id == id),
                "missing provider support entry {id}"
            );
        }
        let mistral = report
            .providers
            .iter()
            .find(|entry| entry.id == "mistral")
            .expect("mistral row");
        assert_eq!(mistral.catalog_provider, "openrouter");
        assert_eq!(mistral.recommended.model, "mistralai/mistral-small-2603");
        assert_eq!(mistral.empirical.status, "not_recorded");

        let azure = report
            .providers
            .iter()
            .find(|entry| entry.id == "azure_openai")
            .expect("azure row");
        assert_ne!(azure.recommended.model, "*");
        assert!(azure.capabilities.native_tools);
        assert!(
            azure.capabilities.batch_api,
            "Azure OpenAI should surface Batch API support"
        );

        let openai = report
            .providers
            .iter()
            .find(|entry| entry.id == "openai")
            .expect("openai row");
        assert_eq!(openai.recommended.model, "gpt-5.4-mini");
        assert!(
            openai
                .capabilities
                .serving_tiers
                .iter()
                .any(|tier| tier.id == "flex" && tier.economics == "discounted"),
            "OpenAI support row should surface synchronous Flex separately from Batch"
        );

        let gemini = report
            .providers
            .iter()
            .find(|entry| entry.id == "gemini")
            .expect("gemini row");
        assert!(
            gemini
                .capabilities
                .serving_tiers
                .iter()
                .any(|tier| tier.id == "priority"
                    && tier.request_value.as_deref() == Some("priority")),
            "Gemini support row should surface Priority as a synchronous serving tier"
        );
    }

    #[test]
    fn curated_recommendations_do_not_point_at_superseded_models() {
        let report = build_report(Path::new(DEFAULT_NOTES_PATH), &[]).expect("report");
        for entry in &report.providers {
            if entry.recommended.model == "*" || entry.recommended.model.contains('*') {
                continue;
            }
            let model = harn_vm::llm_config::model_catalog_entry(&entry.recommended.model)
                .unwrap_or_else(|| panic!("{} recommends missing catalog model", entry.id));
            assert_eq!(
                model.provider, entry.catalog_provider,
                "{} recommends {} from provider {}, not {}",
                entry.id, entry.recommended.model, model.provider, entry.catalog_provider
            );
            assert!(
                model.superseded_by.is_none(),
                "{} recommends superseded model {} -> {:?}",
                entry.id,
                entry.recommended.model,
                model.superseded_by
            );
        }
    }

    #[test]
    fn empirical_summary_attaches_to_matching_model() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let summary = tmp.path().join("summary.json");
        fs::write(
            &summary,
            r#"{
              "runs": [
                {
                  "run_id": "python-add__openrouter_mistral-small__native",
                  "selector": {"provider": "openrouter", "model": "mistralai/mistral-small-2603"},
                  "tool_format": "native",
                  "status": "passed",
                  "passed": true,
                  "skipped": false
                },
                {
                  "run_id": "python-add__openrouter_mistral-small__text",
                  "selector": {"provider": "openrouter", "model": "mistralai/mistral-small-2603"},
                  "tool_format": "text",
                  "status": "failed",
                  "passed": false,
                  "skipped": false
                }
              ],
              "comparisons": [
                {
                  "selector": {"provider": "openrouter", "model": "mistralai/mistral-small-2603"},
                  "equivalent": false
                }
              ]
            }"#,
        )
        .expect("write summary");

        let report = build_report(Path::new(DEFAULT_NOTES_PATH), &[summary]).expect("report");
        let mistral = report
            .providers
            .iter()
            .find(|entry| entry.id == "mistral")
            .expect("mistral row");
        assert_eq!(mistral.empirical.total_runs, 2);
        assert_eq!(mistral.empirical.passed_runs, 1);
        assert_eq!(
            mistral.empirical.best_tool_format.as_deref(),
            Some("native")
        );
        assert_eq!(
            mistral.empirical.native_text_parity.as_deref(),
            Some("diverged")
        );
        assert_eq!(mistral.empirical.sources, vec!["summary.json"]);

        let openrouter = report
            .providers
            .iter()
            .find(|entry| entry.id == "openrouter")
            .expect("openrouter row");
        assert_eq!(openrouter.empirical.status, "not_recorded");
    }

    #[test]
    fn empirical_summary_matches_model_patterns() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let summary = tmp.path().join("summary.json");
        fs::write(
            &summary,
            r#"{
              "runs": [
                {
                  "run_id": "azure-gpt4o-native",
                  "selector": {"provider": "azure_openai", "model": "gpt-4o"},
                  "tool_format": "native",
                  "status": "passed",
                  "passed": true,
                  "skipped": false
                }
              ]
            }"#,
        )
        .expect("write summary");

        let report = build_report(Path::new(DEFAULT_NOTES_PATH), &[summary]).expect("report");
        let azure = report
            .providers
            .iter()
            .find(|entry| entry.id == "azure_openai")
            .expect("azure row");
        assert_eq!(azure.recommended.model, "gpt-*");
        assert_eq!(azure.empirical.status, "observed_pass");
        assert_eq!(azure.empirical.total_runs, 1);
    }

    #[test]
    fn markdown_and_json_render_generated_surfaces() {
        let report = build_report(Path::new(DEFAULT_NOTES_PATH), &[]).expect("report");
        let markdown = render_markdown(&report);
        assert!(markdown.contains("GENERATED by `harn provider catalog support`"));
        assert!(markdown.contains("Provider support recommendations"));
        assert!(!markdown.contains("API_KEY="));

        let json = render_json(&report).expect("json");
        let parsed: JsonValue = serde_json::from_str(&json).expect("valid json");
        assert_eq!(parsed["schema_version"], PROVIDER_SUPPORT_SCHEMA_VERSION);
        assert!(parsed["providers"]
            .as_array()
            .is_some_and(|rows| rows.len() >= 6));
    }
}
