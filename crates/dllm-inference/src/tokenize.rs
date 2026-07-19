//! Tokenize / detokenize.

use crate::{InferenceError, InferenceModel};
use llama_cpp_4::prelude::*;
// Override the llama.cpp prelude's `Result<T>` alias with the std one.
use std::result::Result;

/// One token id, with its decoded piece when the caller requested pieces.
#[derive(Debug, Clone)]
pub struct TokenPiece {
    pub id: i32,
    pub piece: Option<String>,
}

impl InferenceModel {
    /// Tokenizes `content`. `add_special` controls BOS insertion; `with_pieces`
    /// fills each [`TokenPiece::piece`] with the decoded string.
    pub fn tokenize(
        &self,
        content: &str,
        add_special: bool,
        with_pieces: bool,
    ) -> Result<Vec<TokenPiece>, InferenceError> {
        let add_bos = if add_special {
            AddBos::Always
        } else {
            AddBos::Never
        };
        let tokens = self
            .model
            .str_to_token(content, add_bos)
            .map_err(|e| InferenceError::invalid(format!("tokenize failed: {e}")))?;
        Ok(tokens
            .iter()
            .map(|tok| {
                let piece = if with_pieces {
                    Some(
                        self.model
                            .token_to_str(*tok, Special::Plaintext)
                            .unwrap_or_default(),
                    )
                } else {
                    None
                };
                TokenPiece { id: tok.0, piece }
            })
            .collect())
    }

    /// Detokenizes token ids back into a string.
    pub fn detokenize(&self, ids: &[i32]) -> Result<String, InferenceError> {
        let token_ids: Vec<LlamaToken> = ids.iter().map(|&id| LlamaToken(id)).collect();
        self.model
            .detokenize(&token_ids, false, true)
            .map_err(|e| InferenceError::invalid(format!("detokenize failed: {e}")))
    }
}
