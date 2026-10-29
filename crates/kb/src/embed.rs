//! Local text embeddings with bge-m3 (ONNX, quantized) on CPU.
//!
//! The dense bge-m3 embedding is the L2-normalized hidden state of the
//! first (`<s>`/CLS) token, 1024 dimensions. Inference runs on Tokio's
//! blocking pool, never on the async executor. Inputs are truncated to
//! 512 tokens, which is plenty for ~300-token chunks and spoken queries.

use std::{
    path::Path,
    sync::{Arc, Mutex},
};

use tokenizers::Tokenizer;

use crate::KbError;

/// Embedding dimension of bge-m3.
pub const DIM: usize = 1024;
const MAX_TOKENS: usize = 512;

/// bge-m3 embedder.
pub struct Embedder {
    session: Mutex<ort::session::Session>,
    tokenizer: Tokenizer,
    pad_id: u32,
    wants_token_type_ids: bool,
}

impl std::fmt::Debug for Embedder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Embedder").finish_non_exhaustive()
    }
}

impl Embedder {
    /// Loads `model_quantized.onnx` and `tokenizer.json` from `dir`.
    pub fn load(dir: &Path, threads: usize) -> Result<Self, KbError> {
        let model = dir.join("model_quantized.onnx");
        let tok = dir.join("tokenizer.json");
        let e = |m: String| KbError::Embed(m);
        let session = ort::session::Session::builder()
            .and_then(|b| {
                b.with_optimization_level(ort::session::builder::GraphOptimizationLevel::Level3)
            })
            .and_then(|b| b.with_intra_threads(threads.max(1)))
            .and_then(|b| b.commit_from_file(&model))
            .map_err(|x| e(format!("{}: {x}", model.display())))?;
        let wants_token_type_ids = session.inputs.iter().any(|i| i.name == "token_type_ids");
        let tokenizer =
            Tokenizer::from_file(&tok).map_err(|x| e(format!("{}: {x}", tok.display())))?;
        let pad_id = tokenizer.token_to_id("<pad>").unwrap_or(1);
        Ok(Self {
            session: Mutex::new(session),
            tokenizer,
            pad_id,
            wants_token_type_ids,
        })
    }

    /// Token count of `text` (without special tokens).
    pub fn count_tokens(&self, text: &str) -> usize {
        self.tokenizer
            .encode(text, false)
            .map(|e| e.get_ids().len())
            .unwrap_or_else(|_| text.split_whitespace().count())
    }

    /// Embeds a batch synchronously (call from a blocking thread).
    pub fn embed_blocking(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, KbError> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }
        let mut ids: Vec<Vec<u32>> = Vec::with_capacity(texts.len());
        for t in texts {
            let enc = self
                .tokenizer
                .encode(t.as_str(), true)
                .map_err(|e| KbError::Embed(e.to_string()))?;
            let mut v = enc.get_ids().to_vec();
            v.truncate(MAX_TOKENS);
            ids.push(v);
        }
        let batch = ids.len();
        let len = ids.iter().map(Vec::len).max().unwrap_or(1).max(1);
        let mut input_ids = Vec::with_capacity(batch * len);
        let mut mask = Vec::with_capacity(batch * len);
        for row in &ids {
            for i in 0..len {
                match row.get(i) {
                    Some(&id) => {
                        input_ids.push(i64::from(id));
                        mask.push(1i64);
                    }
                    None => {
                        input_ids.push(i64::from(self.pad_id));
                        mask.push(0i64);
                    }
                }
            }
        }
        let err = |e: ort::Error| KbError::Embed(e.to_string());
        let ids_t = ort::value::Tensor::from_array(([batch, len], input_ids)).map_err(err)?;
        let mask_t = ort::value::Tensor::from_array(([batch, len], mask)).map_err(err)?;
        let mut session = self
            .session
            .lock()
            .map_err(|_| KbError::Embed("embedder lock poisoned".into()))?;
        let outputs = if self.wants_token_type_ids {
            let types = ort::value::Tensor::from_array(([batch, len], vec![0i64; batch * len]))
                .map_err(err)?;
            session.run(ort::inputs![
                "input_ids" => ids_t,
                "attention_mask" => mask_t,
                "token_type_ids" => types
            ])
        } else {
            session.run(ort::inputs!["input_ids" => ids_t, "attention_mask" => mask_t])
        }
        .map_err(err)?;

        let (shape, data) = match outputs.get("sentence_embedding") {
            Some(v) => v.try_extract_tensor::<f32>().map_err(err)?,
            None => outputs[0].try_extract_tensor::<f32>().map_err(err)?,
        };
        let dims: Vec<usize> = shape.iter().map(|&d| d.max(0) as usize).collect();
        let mut out = Vec::with_capacity(batch);
        for b in 0..batch {
            let row = match dims.as_slice() {
                // [batch, dim]: already pooled.
                [_, d] => &data[b * d..(b + 1) * d],
                // [batch, seq, dim]: take the first token.
                [_, s, d] => &data[b * s * d..b * s * d + d],
                _ => return Err(KbError::Embed(format!("unexpected output shape {dims:?}"))),
            };
            out.push(normalize(row));
        }
        Ok(out)
    }

    /// Embeds a batch on the blocking pool.
    pub async fn embed(self: &Arc<Self>, texts: Vec<String>) -> Result<Vec<Vec<f32>>, KbError> {
        let this = Arc::clone(self);
        tokio::task::spawn_blocking(move || this.embed_blocking(&texts))
            .await
            .map_err(|e| KbError::Embed(e.to_string()))?
    }
}

fn normalize(v: &[f32]) -> Vec<f32> {
    let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt().max(1e-12);
    v.iter().map(|x| x / norm).collect()
}

/// Formats a vector as a pgvector text literal (`[0.1,0.2,...]`).
pub fn to_pgvector(v: &[f32]) -> String {
    let mut s = String::with_capacity(v.len() * 10 + 2);
    s.push('[');
    for (i, x) in v.iter().enumerate() {
        if i > 0 {
            s.push(',');
        }
        s.push_str(&x.to_string());
    }
    s.push(']');
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pgvector_literal() {
        assert_eq!(to_pgvector(&[0.5, -1.0, 0.0]), "[0.5,-1,0]");
        let n = normalize(&[3.0, 4.0]);
        assert!((n[0] - 0.6).abs() < 1e-6 && (n[1] - 0.8).abs() < 1e-6);
    }

    #[test]
    fn real_model_if_present() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../models/bge-m3");
        if !dir.join("model_quantized.onnx").exists() {
            eprintln!("skipping: bge-m3 not downloaded");
            return;
        }
        let e = Embedder::load(&dir, 2).unwrap();
        let v = e
            .embed_blocking(&[
                "clinic advertising price".to_owned(),
                "क्लिनिक विज्ञापन की कीमत".to_owned(),
                "weather in Paris".to_owned(),
            ])
            .unwrap();
        assert_eq!(v[0].len(), DIM);
        let dot = |a: &[f32], b: &[f32]| a.iter().zip(b).map(|(x, y)| x * y).sum::<f32>();
        // Cross-lingual match beats an unrelated sentence.
        assert!(dot(&v[0], &v[1]) > dot(&v[0], &v[2]));
    }
}
