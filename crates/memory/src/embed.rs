//! Abstração de embeddings.
//!
//! O embedder de produção (fastembed, com modelo multilíngue) mora no
//! `orchestrator-memoryd` — o único processo que carrega modelo. Aqui fica o
//! contrato e um embedder determinístico para testes, sem download.

use anyhow::Result;

/// Abstração sobre um gerador de embeddings de texto.
pub trait Embedder: Send + Sync {
    /// Gera um vetor de embedding para cada texto.
    fn embed(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>>;

    /// Dimensão dos vetores produzidos.
    fn dim(&self) -> usize;

    /// Nome do modelo (gravado junto à indexação, para reindexar se mudar).
    fn model_name(&self) -> &str;
}

/// Embedder determinístico baseado em hashing, para testes.
///
/// Cada token contribui para posições do vetor derivadas de um hash simples;
/// textos iguais produzem sempre o mesmo vetor e textos com tokens em comum
/// têm similaridade de cosseno maior.
pub struct HashEmbedder {
    dim: usize,
}

impl HashEmbedder {
    pub fn new(dim: usize) -> Self {
        Self { dim }
    }
}

impl Default for HashEmbedder {
    fn default() -> Self {
        Self::new(64)
    }
}

fn fnv1a(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf29ce484222325;
    for b in bytes {
        hash ^= u64::from(*b);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

impl Embedder for HashEmbedder {
    fn embed(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>> {
        Ok(texts
            .iter()
            .map(|text| {
                let mut v = vec![0.0f32; self.dim];
                for token in text.to_lowercase().split_whitespace() {
                    let h = fnv1a(token.as_bytes());
                    let idx = (h % self.dim as u64) as usize;
                    let sign = if (h >> 32) % 2 == 0 { 1.0 } else { -1.0 };
                    v[idx] += sign;
                }
                let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
                if norm > 0.0 {
                    for x in &mut v {
                        *x /= norm;
                    }
                }
                v
            })
            .collect())
    }

    fn dim(&self) -> usize {
        self.dim
    }

    fn model_name(&self) -> &str {
        "hash-test-embedder"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rerank::cosine_similarity;

    #[test]
    fn hash_embedder_is_deterministic() {
        let e = HashEmbedder::default();
        let a = e.embed(&["hello world"]).unwrap();
        let b = e.embed(&["hello world"]).unwrap();
        assert_eq!(a, b);
        assert_eq!(a[0].len(), e.dim());
    }

    #[test]
    fn similar_texts_are_closer() {
        let e = HashEmbedder::default();
        let vs = e
            .embed(&["rust sqlite database", "rust sqlite storage", "banana smoothie recipe"])
            .unwrap();
        assert!(cosine_similarity(&vs[0], &vs[1]) > cosine_similarity(&vs[0], &vs[2]));
    }
}
