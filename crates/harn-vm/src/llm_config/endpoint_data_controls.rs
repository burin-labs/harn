use super::{ModelDataControlsDef, ProviderDef};

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
