//! Provider catalog contracts shared by the runtime and host applications.
//! Runtime loading and routing policy remain in `harn-vm`.

pub mod artifact;
pub mod data_controls;
pub mod model_def;
pub mod presentation;

pub use artifact::*;
pub use data_controls::*;
pub use model_def::*;
pub use presentation::*;

#[cfg(test)]
mod tests {
    #[test]
    fn shipped_catalog_roundtrips_without_field_loss() {
        let source = include_str!("../../../spec/provider-catalog/provider-catalog.json");
        let catalog: super::ProviderCatalogArtifact = serde_json::from_str(source).unwrap();
        assert!(!catalog.models.is_empty());
        let expected: serde_json::Value = serde_json::from_str(source).unwrap();
        assert_eq!(serde_json::to_value(catalog).unwrap(), expected);
    }
}
