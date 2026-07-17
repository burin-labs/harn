use std::collections::{BTreeMap, BTreeSet};

use super::*;

const RETIRED_DEEPSEEK_DIRECT_IDS: &[&str] = &["deepseek-chat", "deepseek-reasoner"];

pub fn validate_artifact(artifact: &ProviderCatalogArtifact) -> ProviderCatalogValidation {
    let mut result = ProviderCatalogValidation::default();
    if artifact.schema_version != PROVIDER_CATALOG_SCHEMA_VERSION {
        result.errors.push(format!(
            "schema_version must be {}, got {}",
            PROVIDER_CATALOG_SCHEMA_VERSION, artifact.schema_version
        ));
    }
    if artifact.schema != PROVIDER_CATALOG_SCHEMA_ID {
        result.errors.push(format!(
            "schema must be {}, got {}",
            PROVIDER_CATALOG_SCHEMA_ID, artifact.schema
        ));
    }
    if artifact.providers.is_empty() {
        result.errors.push("catalog has no providers".to_string());
    }
    if artifact.models.is_empty() {
        result.errors.push("catalog has no models".to_string());
    }

    let provider_ids: BTreeSet<_> = artifact.providers.iter().map(|p| p.id.as_str()).collect();
    for provider in &artifact.providers {
        if provider.id.trim().is_empty() {
            result
                .errors
                .push("provider id cannot be empty".to_string());
        }
        if provider.display_name.trim().is_empty() {
            result.errors.push(format!(
                "provider {} display_name cannot be empty",
                provider.id
            ));
        }
        if provider.endpoint.chat_endpoint.trim().is_empty() {
            result.errors.push(format!(
                "provider {} chat_endpoint cannot be empty",
                provider.id
            ));
        }
        if provider.auth.required
            && provider.auth.env.is_empty()
            && provider.auth.style != "aws_sigv4"
        {
            result.errors.push(format!(
                "provider {} requires auth but declares no auth env keys",
                provider.id
            ));
        }
        if let Some(rate_limits) = &provider.rate_limits {
            validate_rate_limits(
                &format!("provider {}", provider.id),
                rate_limits,
                &mut result,
            );
        }
        if let Some(performance) = &provider.performance {
            validate_performance(
                &format!("provider {}", provider.id),
                performance,
                &mut result,
            );
        }
        validate_extra_headers(provider, &mut result);
        if let Some(healthcheck) = &provider.healthcheck {
            validate_provider_healthcheck(provider, healthcheck, &mut result);
        }
        if let Some(local_runtime) = &provider.local_runtime {
            super::local_runtime::validate(&provider.id, local_runtime, &mut result);
        }
    }

    let mut alias_names = BTreeSet::new();
    for alias in &artifact.aliases {
        if alias.name.trim().is_empty() {
            result.errors.push("alias name cannot be empty".to_string());
        }
        if !alias_names.insert(alias.name.as_str()) {
            result
                .errors
                .push(format!("duplicate alias name {}", alias.name));
        }
        if !provider_ids.contains(alias.provider.as_str()) {
            result.errors.push(format!(
                "alias {} references unknown provider {}",
                alias.name, alias.provider
            ));
        }
    }

    let mut model_ids = BTreeSet::new();
    let mut model_pairs = BTreeSet::new();
    let mut dispatch_pairs = BTreeSet::new();
    for model in &artifact.models {
        if !model_ids.insert(model.id.as_str()) {
            result
                .errors
                .push(format!("duplicate model id {}", model.id));
        }
        model_pairs.insert((model.provider.as_str(), model.id.as_str()));
        if model.deprecation.status == DeprecationStatus::Active {
            dispatch_pairs.insert((
                model.provider.clone(),
                model.wire_model.clone().unwrap_or_else(|| model.id.clone()),
            ));
        }
        if model.name.trim().is_empty() {
            result
                .errors
                .push(format!("model {} name cannot be empty", model.id));
        }
        if model
            .blurb
            .as_deref()
            .is_some_and(|blurb| blurb.trim().is_empty())
        {
            result
                .errors
                .push(format!("model {} blurb cannot be empty", model.id));
        }
        if !provider_ids.contains(model.provider.as_str()) {
            result.errors.push(format!(
                "model {} references unknown provider {}",
                model.id, model.provider
            ));
        }
        validate_token_field(model, "family", &model.family, &mut result);
        validate_token_field(model, "lineage", &model.lineage, &mut result);
        for family in &model.complementary_with {
            validate_token_field(model, "complementary_with", family, &mut result);
        }
        for selector in &model.avoid_as_reviewer_for {
            validate_reviewer_selector(model, selector, &mut result);
        }
        if model.context_window == 0 {
            result.errors.push(format!(
                "model {} context_window must be positive",
                model.id
            ));
        }
        if let Some(pricing) = &model.pricing {
            validate_pricing(model, pricing, &mut result);
        }
        if let Some(rate_limits) = &model.rate_limits {
            validate_rate_limits(&format!("model {}", model.id), rate_limits, &mut result);
        }
        if let Some(performance) = &model.performance {
            validate_performance(&format!("model {}", model.id), performance, &mut result);
        }
        if let Some(architecture) = &model.architecture {
            validate_architecture(model, architecture, &mut result);
        }
        if let Some(memory) = &model.local_memory {
            validate_local_memory(model, memory, &mut result);
        }
        if model.deprecation.status == DeprecationStatus::Deprecated
            && model
                .deprecation
                .note
                .as_deref()
                .unwrap_or("")
                .trim()
                .is_empty()
        {
            result.errors.push(format!(
                "deprecated model {} must include deprecation.note",
                model.id
            ));
        }
        if model.provider == "deepseek" && RETIRED_DEEPSEEK_DIRECT_IDS.contains(&model.id.as_str())
        {
            result.errors.push(format!(
                "direct DeepSeek model {} is retired; use deepseek-v4-flash or deepseek-v4-pro",
                model.id
            ));
        }
        let mut serving_tier_ids = BTreeSet::new();
        for tier in &model.serving_tiers {
            if !serving_tier_ids.insert(tier.id.as_str()) {
                result.errors.push(format!(
                    "model {} declares duplicate serving_tiers id {:?}",
                    model.id, tier.id
                ));
            }
            if let Some(pricing) = &tier.pricing {
                validate_pricing(model, pricing, &mut result);
            }
            if let Some(status) = tier.status.as_deref() {
                if !matches!(status, "ga" | "research_preview" | "deprecated") {
                    result.warnings.push(format!(
                        "model {} serving_tiers[{}].status {:?} is not one of ga|research_preview|deprecated",
                        model.id, tier.id, status
                    ));
                }
            }
            if tier.request.is_none() && tier.id == crate::llm::serving_tiers::FAST_TIER_ID {
                result.errors.push(format!(
                    "model {} fast serving tier must declare a request knob",
                    model.id
                ));
            }
        }
        let has_batch_tag = model.capability_tags.iter().any(|tag| tag == "batch");
        match (&model.batch, has_batch_tag) {
            (Some(batch), true) => validate_batch_support(model, batch, &mut result),
            (Some(_), false) => result.errors.push(format!(
                "model {} declares batch support but capability_tags omits batch",
                model.id
            )),
            (None, true) => result.errors.push(format!(
                "model {} capability_tags includes batch but model.batch is missing",
                model.id
            )),
            (None, false) => {}
        }
    }

    let mut route_pairs = BTreeSet::new();
    for route in &artifact.routing_routes {
        if route.provider.trim().is_empty() {
            result
                .errors
                .push("routing route provider cannot be empty".to_string());
        }
        if route.model.trim().is_empty() {
            result
                .errors
                .push("routing route model cannot be empty".to_string());
        }
        if !provider_ids.contains(route.provider.as_str()) {
            result.errors.push(format!(
                "routing route {}:{} references unknown provider {}",
                route.provider, route.model, route.provider
            ));
        }
        if !route_pairs.insert((route.provider.as_str(), route.model.as_str())) {
            result.errors.push(format!(
                "duplicate routing route {}:{}",
                route.provider, route.model
            ));
        }
        if !dispatch_pairs.contains(&(route.provider.clone(), route.model.clone())) {
            result.errors.push(format!(
                "routing route {}:{} does not match an active catalog model wire route",
                route.provider, route.model
            ));
        }
        if let Some(timeout_ms) = route.timeout_ms {
            if timeout_ms == 0 {
                result.errors.push(format!(
                    "routing route {}:{} timeout_ms must be positive",
                    route.provider, route.model
                ));
            }
        }
        if let Some(family) = route.family.as_deref() {
            validate_route_token(&route.provider, &route.model, "family", family, &mut result);
        }
        for capability in &route.capabilities {
            if capability.trim().is_empty() {
                result.errors.push(format!(
                    "routing route {}:{} capability cannot be empty",
                    route.provider, route.model
                ));
            }
        }
    }

    // Structured supersession pointers must reference a real catalog row so
    // `superseded_by` can be trusted as a migration target by downstream
    // tooling. A dangling pointer is a soft warning (the row is still
    // usable) rather than a hard error, mirroring how `note` is advisory.
    for model in &artifact.models {
        if let Some(target) = model.deprecation.superseded_by.as_deref() {
            if !model_ids.contains(target) {
                result.warnings.push(format!(
                    "model {} declares superseded_by {} with no matching catalog row",
                    model.id, target
                ));
            }
        }
    }

    // Tier is a CAPABILITY of the logical model, not of who hosts it. The
    // model-agnostic routing/escalation layer reads `tier` to decide
    // "already capable, do not escalate" vs "escalate me" — so if the same
    // weights are tiered `frontier` on one provider row and `mid` on another,
    // the identical model gets different escalation eligibility purely by host.
    // Enforce one tier per `equivalence_group` at catalog-build time so the
    // divergence cannot be reintroduced silently. Deprecated rows are excluded
    // (a superseded row may legitimately keep a stale tier until removed).
    {
        let mut tiers_by_group: BTreeMap<&str, BTreeMap<&str, BTreeSet<&str>>> = BTreeMap::new();
        for model in &artifact.models {
            if model.deprecation.status == DeprecationStatus::Deprecated {
                continue;
            }
            let Some(group) = model.equivalence_group.as_deref() else {
                continue;
            };
            if group.trim().is_empty() {
                continue;
            }
            tiers_by_group
                .entry(group)
                .or_default()
                .entry(model.tier.as_str())
                .or_default()
                .insert(model.id.as_str());
        }
        for (group, tiers) in &tiers_by_group {
            if tiers.len() > 1 {
                let detail = tiers
                    .iter()
                    .map(|(tier, ids)| {
                        format!(
                            "{tier} ({})",
                            ids.iter().copied().collect::<Vec<_>>().join(", ")
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("; ");
                result.errors.push(format!(
                    "equivalence_group {group} declares conflicting tiers across its \
                     provider rows: {detail}. tier is a capability of the logical model — \
                     give every active row in the group the same tier (the conservative \
                     least-capable host baseline), not a per-provider value."
                ));
            }
        }
    }

    // GAMING GUARD (L3): within an equivalence_group, a LOCAL-runtime host row
    // must not be decorated with MORE strengths than the least-decorated host in
    // the group. `strengths` feeds the routing layer's "already capable, do not
    // escalate" verdict (a local route claiming "agentic" reads as capable and
    // SUPPRESSES a needed escalation). If a local route inherited a decorated
    // cloud row's strengths it would gain capability it never earned on the
    // local serving stack and inflate apparent local convergence — so a local
    // row's strengths must be a SUBSET of every co-grouped row's strengths (the
    // conservative weights-intrinsic baseline), never a superset. Providers with
    // a `local_runtime` descriptor are the local hosts; this is data-driven, not
    // a hardcoded name list. Deprecated rows are excluded.
    {
        let local_provider_ids: BTreeSet<&str> = artifact
            .providers
            .iter()
            .filter(|p| p.local_runtime.is_some())
            .map(|p| p.id.as_str())
            .collect();
        // Group active rows by equivalence_group, keeping each row's strengths.
        let mut rows_by_group: BTreeMap<&str, Vec<&CatalogModel>> = BTreeMap::new();
        for model in &artifact.models {
            if model.deprecation.status == DeprecationStatus::Deprecated {
                continue;
            }
            let Some(group) = model.equivalence_group.as_deref() else {
                continue;
            };
            if group.trim().is_empty() {
                continue;
            }
            rows_by_group.entry(group).or_default().push(model);
        }
        for (group, rows) in &rows_by_group {
            // The group's conservative baseline is the intersection of every
            // row's strengths — what holds for the weights regardless of host.
            let mut baseline: Option<BTreeSet<&str>> = None;
            for model in rows {
                let row: BTreeSet<&str> = model.strengths.iter().map(String::as_str).collect();
                baseline = Some(match baseline {
                    None => row,
                    Some(acc) => acc.intersection(&row).copied().collect(),
                });
            }
            let baseline = baseline.unwrap_or_default();
            for model in rows {
                if !local_provider_ids.contains(model.provider.as_str()) {
                    continue;
                }
                let row: BTreeSet<&str> = model.strengths.iter().map(String::as_str).collect();
                let extras: Vec<&str> = row.difference(&baseline).copied().collect();
                if !extras.is_empty() {
                    result.errors.push(format!(
                        "local-runtime row {}/{} in equivalence_group {group} claims strengths \
                         [{}] beyond the group's conservative baseline [{}]. A local route must \
                         not inherit a cloud peer's decoration — strengths must be the \
                         least-capable host baseline (a subset of every co-grouped row), or the \
                         local route reads as already-capable and suppresses real escalations.",
                        model.provider,
                        model.id,
                        extras.join(", "),
                        baseline.iter().copied().collect::<Vec<_>>().join(", "),
                    ));
                }
            }
        }
    }

    // Index models by (provider, id) so alias tool_format can be checked
    // against the target model's declared tool support. An alias is the one
    // place a harness author can pin `native` / `text` per model, so a typo
    // or a format the model can't serve must be caught at catalog-build time
    // rather than silently degrading at call time.
    let model_by_pair: BTreeMap<(&str, &str), &CatalogModel> = artifact
        .models
        .iter()
        .map(|model| ((model.provider.as_str(), model.id.as_str()), model))
        .collect();

    let dedicated_pairs: BTreeSet<(&str, &str)> = artifact
        .models
        .iter()
        .filter(|model| model.availability == ModelAvailabilityStatus::Dedicated)
        .map(|model| (model.provider.as_str(), model.id.as_str()))
        .collect();
    for alias in &artifact.aliases {
        if !model_pairs.contains(&(alias.provider.as_str(), alias.model_id.as_str())) {
            result.errors.push(format!(
                "alias {} targets {}/{} without a catalog row",
                alias.name, alias.provider, alias.model_id
            ));
        }
        if let Some(format) = alias.tool_format.as_deref() {
            // `json` (fenced-JSON) and `text` (tagged/heredoc) are both
            // TEXT-channel formats and validate against `tool_support.text`;
            // `native` validates against `tool_support.native`.
            if format != "native" && format != "text" && format != "json" {
                result.errors.push(format!(
                    "alias {} declares tool_format {:?}; must be \"native\", \"text\", or \"json\"",
                    alias.name, format
                ));
            } else if let Some(model) =
                model_by_pair.get(&(alias.provider.as_str(), alias.model_id.as_str()))
            {
                if format == "native" && !model.tool_support.native {
                    result.errors.push(format!(
                        "alias {} pins tool_format \"native\" but model {}/{} does not support native tool calling",
                        alias.name, alias.provider, alias.model_id
                    ));
                }
                if (format == "text" || format == "json") && !model.tool_support.text {
                    result.errors.push(format!(
                        "alias {} pins tool_format {:?} (a text-channel format) but model {}/{} does not support text tool calling",
                        alias.name, format, alias.provider, alias.model_id
                    ));
                }
            }
        }
        if is_tier_alias(&alias.name)
            && dedicated_pairs.contains(&(alias.provider.as_str(), alias.model_id.as_str()))
        {
            result.warnings.push(format!(
                "tier alias {} targets dedicated-only model {}/{}; serverless callers will fail until the dedicated endpoint is provisioned",
                alias.name, alias.provider, alias.model_id
            ));
        }
    }

    for variant in &artifact.variants {
        if variant.id.trim().is_empty() {
            result.errors.push("variant id cannot be empty".to_string());
        }
        if !provider_ids.contains(variant.provider.as_str()) {
            result.errors.push(format!(
                "variant {} references unknown provider {}",
                variant.id, variant.provider
            ));
        }
        if !model_pairs.contains(&(variant.provider.as_str(), variant.model_id.as_str())) {
            result.errors.push(format!(
                "variant {} targets {}/{} without a catalog row",
                variant.id, variant.provider, variant.model_id
            ));
        }
    }

    validate_model_families(artifact, &provider_ids, &mut result);

    result
}

pub fn validate_current() -> ProviderCatalogValidation {
    validate_artifact(&artifact())
}

fn validate_model_families(
    artifact: &ProviderCatalogArtifact,
    provider_ids: &BTreeSet<&str>,
    result: &mut ProviderCatalogValidation,
) {
    let models_by_id: BTreeMap<&str, &CatalogModel> = artifact
        .models
        .iter()
        .map(|model| (model.id.as_str(), model))
        .collect();
    let mut family_ids = BTreeSet::new();

    for family in &artifact.families {
        let context = format!("model family {}", family.id);
        if !family_ids.insert(family.id.as_str()) {
            result
                .errors
                .push(format!("duplicate model family id {}", family.id));
        }
        validate_presentation_token(&context, "id", &family.id, false, result);
        validate_nonempty(&context, "label", &family.label, result);
        validate_nonempty(
            &context,
            "plain_description",
            &family.plain_description,
            result,
        );
        if !provider_ids.contains(family.provider.as_str()) {
            result.errors.push(format!(
                "{context} references unknown provider {}",
                family.provider
            ));
        }
        if !(1..=2).contains(&family.dimensions.len()) {
            result.errors.push(format!(
                "{context} must declare one or two dimensions, got {}",
                family.dimensions.len()
            ));
        }
        if family.presets.is_empty() {
            result
                .errors
                .push(format!("{context} must declare at least one preset"));
        }

        let mut dimension_keys = BTreeSet::new();
        let mut model_dimension = None;
        let mut reasoning_dimension = None;
        let mut referenced_model_ids = Vec::new();
        if let Some(model_id) = family.model_id.as_deref() {
            referenced_model_ids.push(model_id);
        }

        for dimension in &family.dimensions {
            let dimension_context = format!("{context} dimension {}", dimension.key);
            validate_presentation_token(&dimension_context, "key", &dimension.key, true, result);
            if !dimension_keys.insert(dimension.key.as_str()) {
                result.errors.push(format!(
                    "{context} declares duplicate dimension key {}",
                    dimension.key
                ));
            }
            validate_nonempty(&dimension_context, "label", &dimension.label, result);
            validate_nonempty(
                &dimension_context,
                "plain_description",
                &dimension.plain_description,
                result,
            );
            if dimension.ordered_values.is_empty() {
                result
                    .errors
                    .push(format!("{dimension_context} must declare ordered_values"));
            }

            match dimension.kind {
                llm_config::ModelFamilyDimensionKind::Model => {
                    if model_dimension.replace(dimension).is_some() {
                        result
                            .errors
                            .push(format!("{context} may declare at most one model dimension"));
                    }
                }
                llm_config::ModelFamilyDimensionKind::ReasoningEffort => {
                    if reasoning_dimension.replace(dimension).is_some() {
                        result.errors.push(format!(
                            "{context} may declare at most one reasoning_effort dimension"
                        ));
                    }
                }
            }

            let mut values = BTreeSet::new();
            for value in &dimension.ordered_values {
                let value_context = format!("{dimension_context} value {}", value.value);
                validate_presentation_token(&value_context, "value", &value.value, true, result);
                if !values.insert(value.value.as_str()) {
                    result.errors.push(format!(
                        "{dimension_context} declares duplicate value {}",
                        value.value
                    ));
                }
                validate_nonempty(&value_context, "label", &value.label, result);
                validate_nonempty(
                    &value_context,
                    "plain_description",
                    &value.plain_description,
                    result,
                );
                for (field, hint) in [
                    ("relative_cost_hint", value.relative_cost_hint),
                    ("relative_speed_hint", value.relative_speed_hint),
                ] {
                    if !(1..=5).contains(&hint) {
                        result.errors.push(format!(
                            "{value_context} {field} must be between 1 and 5, got {hint}"
                        ));
                    }
                }
                match dimension.kind {
                    llm_config::ModelFamilyDimensionKind::Model => {
                        if let Some(model_id) = value.model_id.as_deref() {
                            referenced_model_ids.push(model_id);
                        } else {
                            result.errors.push(format!(
                                "{value_context} on a model dimension must declare model_id"
                            ));
                        }
                    }
                    llm_config::ModelFamilyDimensionKind::ReasoningEffort => {
                        if value.model_id.is_some() {
                            result.errors.push(format!(
                                "{value_context} on a reasoning_effort dimension must not declare model_id"
                            ));
                        }
                    }
                }
            }
        }

        match (model_dimension.is_some(), family.model_id.is_some()) {
            (true, true) => result.errors.push(format!(
                "{context} must not declare model_id when a model dimension selects the model"
            )),
            (false, false) => result.errors.push(format!(
                "{context} without a model dimension must declare model_id"
            )),
            _ => {}
        }

        for model_id in &referenced_model_ids {
            match models_by_id.get(model_id) {
                Some(model) if model.provider != family.provider => result.errors.push(format!(
                    "{context} model {} belongs to provider {}, not {}",
                    model.id, model.provider, family.provider
                )),
                Some(_) => {}
                None => result
                    .errors
                    .push(format!("{context} references unknown model {model_id}")),
            }
        }

        if let Some(dimension) = reasoning_dimension {
            for value in &dimension.ordered_values {
                let supported_somewhere = referenced_model_ids.iter().any(|model_id| {
                    models_by_id.get(model_id).is_some_and(|model| {
                        model
                            .reasoning
                            .effort_levels
                            .iter()
                            .any(|level| level == &value.value)
                    })
                });
                if !supported_somewhere {
                    result.errors.push(format!(
                        "{context} reasoning effort {} is unsupported by every referenced model",
                        value.value
                    ));
                }
            }
        }

        let mut preset_ids = BTreeSet::new();
        for preset in &family.presets {
            let preset_context = format!("{context} preset {}", preset.id);
            validate_presentation_token(&preset_context, "id", &preset.id, true, result);
            if !preset_ids.insert(preset.id.as_str()) {
                result.errors.push(format!(
                    "{context} declares duplicate preset id {}",
                    preset.id
                ));
            }
            validate_nonempty(&preset_context, "label", &preset.label, result);
            validate_nonempty(&preset_context, "plain_blurb", &preset.plain_blurb, result);
            if preset.coordinates.len() != family.dimensions.len()
                || dimension_keys
                    .iter()
                    .any(|key| !preset.coordinates.contains_key(*key))
            {
                result.errors.push(format!(
                    "{preset_context} coordinates must name every dimension exactly once"
                ));
                continue;
            }

            let mut selected_model_id = family.model_id.as_deref();
            for dimension in &family.dimensions {
                let coordinate = preset
                    .coordinates
                    .get(&dimension.key)
                    .expect("coordinate keys checked above");
                let selected_value = dimension
                    .ordered_values
                    .iter()
                    .find(|value| value.value == *coordinate);
                let Some(selected_value) = selected_value else {
                    result.errors.push(format!(
                        "{preset_context} selects unknown {} value {}",
                        dimension.key, coordinate
                    ));
                    continue;
                };
                if dimension.kind == llm_config::ModelFamilyDimensionKind::Model {
                    selected_model_id = selected_value.model_id.as_deref();
                }
            }

            if let (Some(model_id), Some(dimension)) = (selected_model_id, reasoning_dimension) {
                let effort = preset
                    .coordinates
                    .get(&dimension.key)
                    .expect("coordinate keys checked above");
                if let Some(model) = models_by_id.get(model_id) {
                    if !model
                        .reasoning
                        .effort_levels
                        .iter()
                        .any(|level| level == effort)
                    {
                        result.errors.push(format!(
                            "{preset_context} selects unsupported effort {effort} for model {model_id}"
                        ));
                    }
                }
            }
        }
    }
}

fn validate_nonempty(
    context: &str,
    field: &str,
    value: &str,
    result: &mut ProviderCatalogValidation,
) {
    if value.trim().is_empty() {
        result
            .errors
            .push(format!("{context} {field} cannot be empty"));
    }
}

fn validate_presentation_token(
    context: &str,
    field: &str,
    value: &str,
    allow_underscore: bool,
    result: &mut ProviderCatalogValidation,
) {
    let mut chars = value.chars();
    let valid_start = chars
        .next()
        .is_some_and(|character| character.is_ascii_lowercase() || character.is_ascii_digit());
    let valid_rest = chars.all(|character| {
        character.is_ascii_lowercase()
            || character.is_ascii_digit()
            || character == '-'
            || (allow_underscore && character == '_')
    });
    if !valid_start || !valid_rest {
        result.errors.push(format!(
            "{context} {field} must be a normalized lowercase token, got {value:?}"
        ));
    }
}
