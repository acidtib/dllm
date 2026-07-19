//! Dense embeddings.

use crate::{InferenceError, InferenceModel};
use llama_cpp_4::prelude::*;
use std::num::NonZeroU32;
// Override the llama.cpp prelude's `Result<T>` alias with the std one.
use std::result::Result;

impl InferenceModel {
    /// Number of tokens `input` produces with special tokens added, used for
    /// usage accounting. Uses the same tokenization as embedding.
    pub fn token_count(&self, input: &str) -> Result<u32, InferenceError> {
        let tokens = self
            .model
            .str_to_token(input, AddBos::Always)
            .map_err(|e| InferenceError::internal(format!("tokenise: {e}")))?;
        Ok(tokens.len() as u32)
    }

    /// Computes an L2-normalized embedding vector for each input string.
    pub fn embed(&self, inputs: &[String]) -> Result<Vec<Vec<f32>>, InferenceError> {
        let n_embd = self.model.n_embd() as usize;
        let mut results = Vec::with_capacity(inputs.len());

        for input in inputs {
            let tokens = self
                .model
                .str_to_token(input, AddBos::Always)
                .map_err(|e| InferenceError::internal(format!("tokenise: {e}")))?;

            let n_ctx = (tokens.len() as u32 + 16).max(64);
            let ctx_params = LlamaContextParams::default()
                .with_n_ctx(NonZeroU32::new(n_ctx))
                .with_n_batch(n_ctx)
                .with_embeddings(true);

            let mut ctx = self
                .model
                .new_context(&self.backend, ctx_params)
                .map_err(|e| InferenceError::internal(format!("context init: {e}")))?;

            let mut batch = LlamaBatch::new(n_ctx as usize, 1);
            let last = tokens.len().saturating_sub(1) as i32;
            for (i, &tok) in tokens.iter().enumerate() {
                batch
                    .add(tok, i as i32, &[0], i as i32 == last)
                    .map_err(|e| InferenceError::internal(format!("batch add: {e}")))?;
            }
            ctx.decode(&mut batch)
                .map_err(|e| InferenceError::internal(format!("decode: {e}")))?;

            // Try sequence-level pooled embedding first, fall back to last-token.
            let vec = if let Ok(emb) = ctx.embeddings_seq_ith(0) {
                emb.to_vec()
            } else if let Ok(emb) = ctx.embeddings_ith(last) {
                emb.to_vec()
            } else {
                vec![0.0f32; n_embd]
            };

            let norm = vec.iter().map(|x| x * x).sum::<f32>().sqrt().max(1e-12);
            results.push(vec.into_iter().map(|x| x / norm).collect());
        }

        Ok(results)
    }
}
