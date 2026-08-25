//! Resolve an opt-in local ONNX encoder for hostlib embed, if one is installed.
//!
//! The encoder is never bundled. A missing or unloadable install returns `None`
//! so callers stay on the lexical floor. Rankings from this path do not claim
//! semantic status.

use harn_hostlib::embed::EmbedCapability;

/// Prefer an installed local encoder when the CLI was built with `guard-neural`
/// and the catalog model is on disk; otherwise use environment/lexical resolution.
pub(crate) fn resolve_embed_capability() -> EmbedCapability {
    #[cfg(feature = "guard-neural")]
    {
        if let Some(embedder) = try_load_onnx() {
            return EmbedCapability::with_embedder(embedder);
        }
    }
    EmbedCapability::from_env()
}

#[cfg(feature = "guard-neural")]
fn try_load_onnx() -> Option<std::sync::Arc<dyn harn_hostlib::embed::Embedder>> {
    use harn_guard::{catalog, GuardStore, ModelPurpose, OnnxTextEmbedder};
    use harn_hostlib::embed::Embedder;

    struct InstalledOnnxEmbedder {
        inner: OnnxTextEmbedder,
    }

    impl Embedder for InstalledOnnxEmbedder {
        fn embed(&self, text: &str) -> Vec<f32> {
            self.inner
                .embed(text)
                .unwrap_or_else(|_| vec![0.0; self.inner.dim()])
        }

        fn dim(&self) -> usize {
            self.inner.dim()
        }

        fn name(&self) -> &str {
            "onnx-minilm"
        }

        fn is_semantic(&self) -> bool {
            false
        }
    }

    let home = harn_vm::user_dirs::home_dir()?;
    let store = GuardStore::new(&home);
    let model = catalog::find(ModelPurpose::Embedding, catalog::DEFAULT_EMBEDDING_MODEL)?;
    if !store.is_installed(model.name, ModelPurpose::Embedding) {
        return None;
    }
    let dir = store.model_dir(model.name);
    let inner = OnnxTextEmbedder::load(&dir, model.name, model.format).ok()?;
    Some(std::sync::Arc::new(InstalledOnnxEmbedder { inner }))
}
