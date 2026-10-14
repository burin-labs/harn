//! Effective chat endpoint contract and source layers. Provenance is session
//! metadata, kept outside catalog DTOs and semantic overlay comparisons.

use serde::Serialize;
use std::collections::BTreeMap;

use super::provider_def::ChatWireOverlay;
use super::{ChatApiAdapter, ProviderDef, ProvidersConfig};

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "layer", rename_all = "snake_case")]
pub enum ProviderConfigLayer {
    Embedded,
    HostConfig { path: String },
    UserConfig { path: String },
    RuntimeCatalog,
    ExecutionOverride,
}

impl std::fmt::Display for ProviderConfigLayer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Embedded => f.write_str("embedded"),
            Self::HostConfig { path } => write!(f, "host config {path}"),
            Self::UserConfig { path } => write!(f, "user config {path}"),
            Self::RuntimeCatalog => f.write_str("runtime catalog"),
            Self::ExecutionOverride => f.write_str("execution override"),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct ChatWireSources {
    pub chat_endpoint: Option<ProviderConfigLayer>,
    /// An omitted adapter is reset by the layer declaring the endpoint.
    pub chat_api_adapter: Option<ProviderConfigLayer>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProviderChatWireContract {
    pub chat_endpoint: String,
    /// `model_default` means the provider/model dialect selects the wire format.
    pub chat_api_adapter: ChatApiAdapter,
    pub sources: ChatWireSources,
}

impl ChatWireSources {
    pub(super) fn apply(&mut self, definition: &ProviderDef, layer: &ProviderConfigLayer) {
        match definition.chat_wire_overlay() {
            ChatWireOverlay::Endpoint => {
                self.chat_endpoint = Some(layer.clone());
                self.chat_api_adapter = Some(layer.clone());
            }
            ChatWireOverlay::Adapter => self.chat_api_adapter = Some(layer.clone()),
            ChatWireOverlay::Inherit => {}
        }
    }

    pub(super) fn contract(self, definition: &ProviderDef) -> ProviderChatWireContract {
        ProviderChatWireContract {
            chat_endpoint: definition.chat_endpoint.clone(),
            chat_api_adapter: definition
                .chat_api_adapter
                .unwrap_or(ChatApiAdapter::ModelDefault),
            sources: self,
        }
    }
}

pub(super) type ProviderChatWireSources = BTreeMap<String, ChatWireSources>;

pub(super) fn apply_chat_wire_sources(
    sources: &mut ProviderChatWireSources,
    config: &ProvidersConfig,
    layer: ProviderConfigLayer,
) {
    for (name, definition) in &config.providers {
        sources
            .entry(name.clone())
            .or_default()
            .apply(definition, &layer);
    }
}
