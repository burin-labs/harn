//! On-device ONNX inference backend (Layer 2, slice 2b).
//!
//! Compiled only under the `neural` feature. Loads an installed model package
//! (`<state>/guard/<name>/`) — an ONNX graph plus its `tokenizer.json` and
//! `config.json` — and scores untrusted text with a transformer
//! sequence-classifier, implementing [`InjectionClassifier`].
//!
//! On any per-call inference error it degrades to the built-in heuristic so an
//! enabled detector is never silently lost; a load-time failure leaves the
//! heuristic registered (the caller treats `load` errors as "stay on heuristic").

use std::path::Path;
use std::sync::Mutex;

use harn_vm::security::{HeuristicClassifier, InjectionClassifier};
use ort::session::{builder::GraphOptimizationLevel, Session};
use ort::value::Tensor;
use tokenizers::Tokenizer;

use crate::catalog::ModelFormat;
use crate::error::{GuardError, Result};
use crate::neural_math::{resolve_injection_label, softmax};

/// Maximum tokens fed to the model. Transformer injection classifiers are
/// trained at 512; longer untrusted spans are truncated — the head carries the
/// instruction-override signal, and a tail-only attack still trips the heuristic.
const MAX_TOKENS: usize = 512;

/// Input names a transformer encoder classifier may declare. We supply exactly
/// the subset the loaded graph asks for (DeBERTa wants `token_type_ids`; many
/// exports drop it).
const KNOWN_INPUTS: &[&str] = &["input_ids", "attention_mask", "token_type_ids"];

/// An ONNX transformer sequence-classifier scoring text for prompt injection.
pub struct OnnxInjectionClassifier {
    session: Mutex<Session>,
    tokenizer: Tokenizer,
    model_id: String,
    /// Output index of the malicious/injection class.
    injection_label: usize,
    /// Graph input names, in declaration order, filtered to [`KNOWN_INPUTS`].
    input_names: Vec<String>,
    /// Graph output name carrying the classification logits.
    output_name: String,
    /// Fallback used when a single inference call fails.
    heuristic: HeuristicClassifier,
}

impl OnnxInjectionClassifier {
    /// Load the model package installed at `dir` (must contain `model.onnx`,
    /// `tokenizer.json`, and `config.json`). `model_id` becomes the stable id
    /// surfaced in verdicts/audit trails.
    pub fn load(dir: &Path, model_id: &str, format: ModelFormat) -> Result<Self> {
        if format != ModelFormat::Onnx {
            return Err(GuardError::Inference(format!(
                "neural backend supports ONNX models only; `{model_id}` is {}",
                format.as_str()
            )));
        }

        let tokenizer_path = dir.join("tokenizer.json");
        let tokenizer = load_tokenizer(&tokenizer_path)?;

        let config_path = dir.join("config.json");
        let config_json: serde_json::Value = {
            let bytes = std::fs::read(&config_path).map_err(|source| GuardError::Io {
                path: config_path.clone(),
                source,
            })?;
            serde_json::from_slice(&bytes).map_err(|source| GuardError::Manifest {
                path: config_path,
                source,
            })?
        };
        let num_labels = config_json
            .get("id2label")
            .and_then(|v| v.as_object())
            .map_or(2, serde_json::Map::len)
            .max(1);
        let injection_label = resolve_injection_label(&config_json, num_labels);

        let model_path = dir.join("model.onnx");
        let model_bytes = std::fs::read(&model_path).map_err(|source| GuardError::Io {
            path: model_path,
            source,
        })?;
        let session = Session::builder()
            .map_err(inference_err)?
            .with_optimization_level(GraphOptimizationLevel::Level3)
            .map_err(inference_err)?
            .commit_from_memory(&model_bytes)
            .map_err(inference_err)?;

        let input_names: Vec<String> = session
            .inputs
            .iter()
            .map(|input| input.name.clone())
            .filter(|name| KNOWN_INPUTS.contains(&name.as_str()))
            .collect();
        if !input_names.iter().any(|n| n == "input_ids") {
            return Err(GuardError::Inference(format!(
                "model `{model_id}` does not declare an `input_ids` input"
            )));
        }
        let output_name = session
            .outputs
            .iter()
            .find(|o| o.name == "logits")
            .or_else(|| session.outputs.first())
            .map(|o| o.name.clone())
            .ok_or_else(|| {
                GuardError::Inference(format!("model `{model_id}` declares no outputs"))
            })?;

        Ok(Self {
            session: Mutex::new(session),
            tokenizer,
            model_id: model_id.to_owned(),
            injection_label,
            input_names,
            output_name,
            heuristic: HeuristicClassifier,
        })
    }

    /// Run inference for `text`, returning the malicious-class probability.
    fn score_inner(&self, text: &str) -> Result<f64> {
        let encoding = self
            .tokenizer
            .encode(text, true)
            .map_err(|error| GuardError::Inference(format!("tokenize: {error}")))?;
        let ids: Vec<i64> = encoding.get_ids().iter().map(|&x| i64::from(x)).collect();
        let mask: Vec<i64> = encoding
            .get_attention_mask()
            .iter()
            .map(|&x| i64::from(x))
            .collect();
        let len = ids.len();
        let shape = [1_i64, len as i64];

        let mut inputs: Vec<(std::borrow::Cow<'static, str>, ort::value::DynValue)> =
            Vec::with_capacity(self.input_names.len());
        for name in &self.input_names {
            let tensor = match name.as_str() {
                "attention_mask" => Tensor::from_array((shape, mask.clone())),
                "token_type_ids" => Tensor::from_array((shape, vec![0_i64; len])),
                _ => Tensor::from_array((shape, ids.clone())),
            }
            .map_err(inference_err)?;
            inputs.push((std::borrow::Cow::Owned(name.clone()), tensor.into_dyn()));
        }

        // The `SessionOutputs` borrows the session, so extract the logits while
        // the lock is still held.
        let mut session = self
            .session
            .lock()
            .map_err(|_| GuardError::Inference("inference session lock poisoned".to_owned()))?;
        let outputs = session.run(inputs).map_err(inference_err)?;
        let (shape, data) = outputs[self.output_name.as_str()]
            .try_extract_tensor::<f32>()
            .map_err(inference_err)?;
        let num_labels = shape.last().map_or(data.len(), |d| *d as usize).max(1);
        let logits = &data[..num_labels.min(data.len())];
        let probs = softmax(logits);
        let index = self.injection_label.min(probs.len().saturating_sub(1));
        Ok(f64::from(probs.get(index).copied().unwrap_or(0.0)))
    }
}

impl InjectionClassifier for OnnxInjectionClassifier {
    fn model_id(&self) -> &str {
        &self.model_id
    }

    fn score(&self, text: &str) -> f64 {
        match self.score_inner(text) {
            Ok(score) => score,
            Err(error) => {
                // Degrade to the heuristic rather than silently scoring 0: an
                // enabled detector must keep its precision floor on a transient
                // inference failure.
                eprintln!(
                    "[harn-guard] neural inference failed ({error}); using heuristic for this span"
                );
                self.heuristic.score(text)
            }
        }
    }
}

/// Load `tokenizer.json`, capping sequence length at [`MAX_TOKENS`].
fn load_tokenizer(path: &Path) -> Result<Tokenizer> {
    let mut tokenizer = Tokenizer::from_file(path).map_err(|error| {
        GuardError::Inference(format!("load tokenizer {}: {error}", path.display()))
    })?;
    let truncation = tokenizers::TruncationParams {
        max_length: MAX_TOKENS,
        ..Default::default()
    };
    tokenizer
        .with_truncation(Some(truncation))
        .map_err(|error| GuardError::Inference(format!("configure truncation: {error}")))?;
    Ok(tokenizer)
}

fn inference_err(error: impl std::fmt::Display) -> GuardError {
    GuardError::Inference(error.to_string())
}
