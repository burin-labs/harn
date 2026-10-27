use super::{DataControlsDef, ModelDataControlsDef, ProviderDef, TrainingDefault};

/// An endpoint may have no scoped declarations, an unverified destination,
/// or one verified declaration. These states must not collapse into a bool.
#[derive(Clone)]
pub(crate) enum EndpointDataControls {
    Unscoped,
    Unverified,
    Verified(ModelDataControlsDef),
}

impl EndpointDataControls {
    pub(crate) fn verified(self) -> Option<ModelDataControlsDef> {
        match self {
            Self::Verified(controls) => Some(controls),
            Self::Unscoped | Self::Unverified => None,
        }
    }
}

pub(super) fn endpoint_data_controls(
    provider: &ProviderDef,
    base_url: &str,
) -> EndpointDataControls {
    if !provider
        .regions
        .values()
        .any(|region| region.data_controls.is_some())
    {
        return EndpointDataControls::Unscoped;
    }
    let mut matches = provider
        .regions
        .values()
        .filter(|region| region.base_url == base_url);
    let controls = matches
        .next()
        .and_then(|region| region.data_controls.clone());
    match (controls, matches.next()) {
        (Some(controls), None) => EndpointDataControls::Verified(controls),
        _ => EndpointDataControls::Unverified,
    }
}

pub(crate) struct TrustedDataControls {
    pub endpoint: EndpointDataControls,
    pub provider: Option<DataControlsDef>,
    pub model: Option<ModelDataControlsDef>,
    pub training_default: Option<TrainingDefault>,
}

pub(crate) fn trusted_local_runtime(provider: &str) -> Option<bool> {
    let config = super::trusted_policy_config();
    let trusted = config.providers.get(provider)?;
    let selected = super::provider_config(provider)?;
    if selected.managed_supply != trusted.managed_supply || selected.protocol != trusted.protocol {
        return None;
    }
    Some(trusted.local_runtime.is_some())
}

pub(crate) fn trusted_open_weight(provider: &str, model: &str) -> Option<bool> {
    let config = super::trusted_policy_config();
    if !model_wire_matches_trusted(&config, provider, model) {
        return None;
    }
    super::catalog::model_catalog_id_for_route_with_config(&config, provider, model)
        .and_then(|id| config.models.get(&id))
        .and_then(|row| row.open_weight)
}

fn model_wire_matches_trusted(
    config: &super::ProvidersConfig,
    provider: &str,
    model: &str,
) -> bool {
    let expected_chat_wire = config
        .models
        .get(model)
        .and_then(|row| row.wire_model.as_deref())
        .unwrap_or(model);
    let expected_route_wire = super::catalog::model_catalog_id_for_route_with_config(
        config, provider, model,
    )
    .and_then(|id| {
        config
            .models
            .get(&id)
            .map(|row| row.wire_model.clone().unwrap_or(id))
    });
    let selected_route_wire = super::model_catalog_id_for_route(provider, model)
        .and_then(|id| super::model_catalog_entry(&id).map(|row| row.wire_model.unwrap_or(id)));
    super::wire_model_id(model) == expected_chat_wire && selected_route_wire == expected_route_wire
}

/// Bind host privacy facts to the actual endpoint without trusting a package overlay.
pub(crate) fn trusted_data_controls(provider: &str, model: &str) -> TrustedDataControls {
    let config = super::trusted_policy_config();
    let trusted = config.providers.get(provider);
    let selected = super::provider_config(provider);
    let endpoint = match (trusted, selected.as_ref()) {
        _ if !model_wire_matches_trusted(&config, provider, model) => {
            EndpointDataControls::Unverified
        }
        (Some(trusted), Some(selected)) => {
            let selected_url = super::resolve_base_url(selected);
            match endpoint_data_controls(trusted, &selected_url) {
                EndpointDataControls::Unscoped
                    if selected_url != trusted.base_url
                        && !trusted
                            .regions
                            .values()
                            .any(|region| region.base_url == selected_url) =>
                {
                    EndpointDataControls::Unverified
                }
                scope => scope,
            }
        }
        _ => EndpointDataControls::Unverified,
    };
    let model = super::catalog::model_catalog_id_for_route_with_config(&config, provider, model)
        .and_then(|id| config.models.get(&id))
        .and_then(|row| row.data_controls.clone());
    let provider = trusted.and_then(|definition| definition.data_controls.clone());
    let training_default = if matches!(endpoint, EndpointDataControls::Unverified) {
        None
    } else {
        model
            .as_ref()
            .map(|controls| controls.training_default)
            .or_else(|| {
                endpoint
                    .clone()
                    .verified()
                    .map(|controls| controls.training_default)
            })
            .or_else(|| provider.as_ref().map(|controls| controls.training_default))
    };
    TrustedDataControls {
        endpoint,
        provider,
        model,
        training_default,
    }
}
