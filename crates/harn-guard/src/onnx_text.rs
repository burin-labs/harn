//! Opt-in ONNX text encoder (mean-pool + L2). Compiled only under `neural`.
//!
//! Missing or corrupt weights fail at load; a per-call inference error returns
//! `Err` so the host can keep the lexical floor instead of failing a query.
//! This encoder never claims a semantic ranking capability.

use std::path::Path;
use std::sync::Mutex;

use ort::session::{builder::GraphOptimizationLevel, Session};
use ort::value::Tensor;
use tokenizers::Tokenizer;

use crate::catalog::ModelFormat;
use crate::error::{GuardError, Result};

const MAX_TOKENS: usize = 256;
const KNOWN_INPUTS: &[&str] = &["input_ids", "attention_mask", "token_type_ids"];

/// ONNX transformer encoder that mean-pools token states into a unit vector.
pub struct OnnxTextEmbedder {
    session: Mutex<Session>,
    tokenizer: Tokenizer,
    model_id: String,
    input_names: Vec<String>,
    output_name: String,
    dim: usize,
}

impl OnnxTextEmbedder {
    /// Load `model.onnx` + `tokenizer.json` from an installed catalog directory.
    pub fn load(dir: &Path, model_id: &str, format: ModelFormat) -> Result<Self> {
        if format != ModelFormat::Onnx {
            return Err(GuardError::Inference(format!(
                "text encoder supports ONNX models only; `{model_id}` is {}",
                format.as_str()
            )));
        }

        let tokenizer_path = dir.join("tokenizer.json");
        let tokenizer = load_tokenizer(&tokenizer_path)?;

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
            .inputs()
            .iter()
            .map(|input| input.name().to_owned())
            .filter(|name| KNOWN_INPUTS.contains(&name.as_str()))
            .collect();
        if !input_names.iter().any(|name| name == "input_ids") {
            return Err(GuardError::Inference(format!(
                "model `{model_id}` does not declare an `input_ids` input"
            )));
        }
        let output_name = session
            .outputs()
            .iter()
            .find(|output| {
                matches!(
                    output.name(),
                    "last_hidden_state" | "token_embeddings" | "sentence_embedding"
                )
            })
            .or_else(|| session.outputs().first())
            .map(|output| output.name().to_owned())
            .ok_or_else(|| {
                GuardError::Inference(format!("model `{model_id}` declares no outputs"))
            })?;

        let dim = 384;

        Ok(Self {
            session: Mutex::new(session),
            tokenizer,
            model_id: model_id.to_owned(),
            input_names,
            output_name,
            dim,
        })
    }

    /// Stable catalog id.
    pub fn model_id(&self) -> &str {
        &self.model_id
    }

    /// Output vector length.
    pub fn dim(&self) -> usize {
        self.dim
    }

    /// Mean-pool + L2-normalize `text`. Never panics; returns `Err` on inference failure.
    pub fn embed(&self, text: &str) -> Result<Vec<f32>> {
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
        let mut session = self
            .session
            .lock()
            .map_err(|_| GuardError::Inference("inference session lock poisoned".to_owned()))?;
        embed_from_session(
            &mut session,
            &ids,
            &mask,
            &self.input_names,
            &self.output_name,
        )
    }
}

fn embed_from_session(
    session: &mut Session,
    ids: &[i64],
    mask: &[i64],
    input_names: &[String],
    output_name: &str,
) -> Result<Vec<f32>> {
    let len = ids.len();
    let shape = [1_i64, len as i64];
    let mut inputs: Vec<(std::borrow::Cow<'static, str>, ort::value::DynValue)> =
        Vec::with_capacity(input_names.len());
    for name in input_names {
        let tensor = match name.as_str() {
            "attention_mask" => Tensor::from_array((shape, mask.to_vec())),
            "token_type_ids" => Tensor::from_array((shape, vec![0_i64; len])),
            _ => Tensor::from_array((shape, ids.to_vec())),
        }
        .map_err(inference_err)?;
        inputs.push((std::borrow::Cow::Owned(name.clone()), tensor.into_dyn()));
    }
    let outputs = session.run(inputs).map_err(inference_err)?;
    let (out_shape, data) = outputs[output_name]
        .try_extract_tensor::<f32>()
        .map_err(inference_err)?;
    mean_pool_l2(out_shape, data, mask)
}

fn mean_pool_l2(shape: &[i64], data: &[f32], mask: &[i64]) -> Result<Vec<f32>> {
    // sentence_embedding: [1, dim]
    if shape.len() == 2 {
        let dim = *shape.last().unwrap_or(&0) as usize;
        if dim == 0 || data.len() < dim {
            return Err(GuardError::Inference("encoder output is empty".to_owned()));
        }
        return l2_normalize(&data[..dim]);
    }
    // last_hidden_state: [1, seq, dim]
    if shape.len() != 3 {
        return Err(GuardError::Inference(format!(
            "encoder output rank {} is unsupported",
            shape.len()
        )));
    }
    let seq = shape[1] as usize;
    let dim = shape[2] as usize;
    if dim == 0 {
        return Err(GuardError::Inference("encoder hidden size is 0".to_owned()));
    }
    let mut acc = vec![0.0_f32; dim];
    let mut count = 0.0_f32;
    for (token, &flag) in mask.iter().enumerate().take(seq) {
        if flag == 0 {
            continue;
        }
        let start = token * dim;
        let end = start + dim;
        if end > data.len() {
            break;
        }
        for (dst, src) in acc.iter_mut().zip(&data[start..end]) {
            *dst += *src;
        }
        count += 1.0;
    }
    if count > 0.0 {
        for value in &mut acc {
            *value /= count;
        }
    }
    l2_normalize(&acc)
}

fn l2_normalize(vector: &[f32]) -> Result<Vec<f32>> {
    let norm = vector.iter().map(|value| value * value).sum::<f32>().sqrt();
    if norm <= f32::EPSILON {
        return Ok(vec![0.0; vector.len()]);
    }
    Ok(vector.iter().map(|value| value / norm).collect())
}

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
