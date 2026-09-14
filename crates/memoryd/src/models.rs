//! Os dois modelos do memoryd — multilíngues (o dono escreve em português) e
//! de licença que permite publicar o app:
//!
//! - vetores: `multilingual-e5-small` int8 (MIT; conversão ONNX do Xenova);
//! - reranker: `gte-multilingual-reranker-base` int8 (Apache-2.0; conversão
//!   ONNX da onnx-community).
//!
//! O `jina-reranker-v2-base-multilingual` que vinha antes é CC-BY-NC. A troca
//! foi decidida por medição (`examples/bench.rs`; tabela em TRAVAMENTOS.md):
//! o gte acertou 6/6 e é o único que separa pergunta relevante de pergunta
//! sem relação.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use anyhow::{anyhow, Context, Result};
use fastembed::{
    InitOptionsUserDefined, Pooling, RerankInitOptionsUserDefined, TextEmbedding, TextRerank,
    TokenizerFiles, UserDefinedEmbeddingModel, UserDefinedRerankingModel,
};

use crate::download::ModelFile;

/// Nome gravado em `indexed_model` e no nome da coleção: trocar de modelo
/// reindexa tudo numa coleção nova.
pub const EMBED_MODEL_NAME: &str = "multilingual-e5-small-int8";

/// Dimensão dos vetores do `multilingual-e5-small`.
pub const EMBED_DIM: usize = 384;

/// Pasta local do reranker.
pub const RERANK_MODEL_NAME: &str = "gte-multilingual-reranker-base-int8";

/// Tamanho máximo do documento entregue ao reranker (latência previsível).
const RERANK_DOC_CHARS: usize = 1500;

/// Teto de tokens por texto (memórias são curtas; teto baixo = rápido).
const MAX_TOKENS: usize = 512;

const E5_REPO: &str = "Xenova/multilingual-e5-small";
const E5_COMMIT: &str = "761b726dd34fb83930e26aab4e9ac3899aa1fa78";
const GTE_REPO: &str = "onnx-community/gte-multilingual-reranker-base";
const GTE_COMMIT: &str = "ee64367e35a2db0da46bb6497e13a18f8bd585cb";

const fn arquivo(
    repo: &'static str,
    commit: &'static str,
    path: &'static str,
    local: &'static str,
    sha256: &'static str,
    size: u64,
) -> ModelFile {
    ModelFile { repo, commit, path, local, sha256, size }
}

/// Todos os arquivos, fixados por commit e SHA-256 (conferidos contra o
/// Hugging Face em 2026-09-12).
pub const MODEL_FILES: [ModelFile; 10] = [
    arquivo(E5_REPO, E5_COMMIT, "onnx/model_int8.onnx", "multilingual-e5-small-int8/model_int8.onnx",
        "4d24e2bc01a447951524466ef533e52944bf48509e6552810bcee1a2711cb02c", 118_054_593),
    arquivo(E5_REPO, E5_COMMIT, "tokenizer.json", "multilingual-e5-small-int8/tokenizer.json",
        "0b44a9d7b51c3c62626640cda0e2c2f70fdacdc25bbbd68038369d14ebdf4c39", 17_082_730),
    arquivo(E5_REPO, E5_COMMIT, "config.json", "multilingual-e5-small-int8/config.json",
        "cb99455288675345e1a4f411438d5d0adbba5fbd3a67ea4fb03c015433b996c1", 658),
    arquivo(E5_REPO, E5_COMMIT, "special_tokens_map.json", "multilingual-e5-small-int8/special_tokens_map.json",
        "d05497f1da52c5e09554c0cd874037a083e1dc1b9cfd48034d1c717f1afc07a7", 167),
    arquivo(E5_REPO, E5_COMMIT, "tokenizer_config.json", "multilingual-e5-small-int8/tokenizer_config.json",
        "a1d6bc8734a6f635dc158508bef000f8e2e5a759c7d92f984b2c86e5ff53425b", 443),
    arquivo(GTE_REPO, GTE_COMMIT, "onnx/model_int8.onnx", "gte-multilingual-reranker-base-int8/model_int8.onnx",
        "ccf51dba7f8aa9205753761cfaa68c55f741792501463a3bf25d7e5bcdac7c35", 340_858_200),
    arquivo(GTE_REPO, GTE_COMMIT, "tokenizer.json", "gte-multilingual-reranker-base-int8/tokenizer.json",
        "3ffb37461c391f096759f4a9bbbc329da0f36952f88bab061fcf84940c022e98", 17_082_999),
    arquivo(GTE_REPO, GTE_COMMIT, "config.json", "gte-multilingual-reranker-base-int8/config.json",
        "dfa5713436ecb4616eaa576795c8d3efd1f03122031a1ad4973d0b6b7e7edfd3", 1_578),
    arquivo(GTE_REPO, GTE_COMMIT, "special_tokens_map.json", "gte-multilingual-reranker-base-int8/special_tokens_map.json",
        "8c785abebea9ae3257b61681b4e6fd8365ceafde980c21970d001e834cf10835", 964),
    arquivo(GTE_REPO, GTE_COMMIT, "tokenizer_config.json", "gte-multilingual-reranker-base-int8/tokenizer_config.json",
        "6f00514620aff01ba8b7291b2394e98daca5be264cb743805232d9ae27494b2a", 1_340),
];

/// O E5 foi treinado com prefixos — sem eles a qualidade cai. O fastembed
/// não os aplica, então é aqui.
pub fn passage(text: &str) -> String {
    format!("passage: {text}")
}

/// Prefixo de consulta do E5.
pub fn query(text: &str) -> String {
    format!("query: {text}")
}

/// Onde os modelos ficam — nunca dentro da pasta do projeto (antes aparecia
/// um `.fastembed_cache` no projeto de quem abrisse a CLI ali).
pub fn models_dir() -> PathBuf {
    orchestrator_core::config::default_memory_db_path()
        .parent()
        .map(|d| d.join("models"))
        .unwrap_or_else(|| std::env::temp_dir().join("orchestrator-models"))
}

fn tokenizer(dir: &Path) -> Result<TokenizerFiles> {
    let ler = |f: &str| {
        std::fs::read(dir.join(f)).with_context(|| format!("lendo {}", dir.join(f).display()))
    };
    Ok(TokenizerFiles {
        tokenizer_file: ler("tokenizer.json")?,
        config_file: ler("config.json")?,
        special_tokens_map_file: ler("special_tokens_map.json")?,
        tokenizer_config_file: ler("tokenizer_config.json")?,
    })
}

/// Os modelos carregados.
pub struct Models {
    embedder: Mutex<TextEmbedding>,
    reranker: Mutex<TextRerank>,
}

impl Models {
    /// Carrega de `base` — os arquivos já precisam estar lá, conferidos
    /// ([`crate::download::ensure`] com [`MODEL_FILES`]).
    pub fn load(base: &Path) -> Result<Self> {
        let e5 = base.join(EMBED_MODEL_NAME);
        let pesos = std::fs::read(e5.join("model_int8.onnx"))
            .with_context(|| format!("lendo {}", e5.join("model_int8.onnx").display()))?;
        let embedder = TextEmbedding::try_new_from_user_defined(
            UserDefinedEmbeddingModel::new(pesos, tokenizer(&e5)?).with_pooling(Pooling::Mean),
            InitOptionsUserDefined::new().with_max_length(MAX_TOKENS),
        )
        .context("carregando o modelo de embedding (multilingual-e5-small int8)")?;

        let gte = base.join(RERANK_MODEL_NAME);
        let reranker = TextRerank::try_new_from_user_defined(
            UserDefinedRerankingModel::new(gte.join("model_int8.onnx"), tokenizer(&gte)?),
            RerankInitOptionsUserDefined::new().with_max_length(MAX_TOKENS),
        )
        .context("carregando o reranker (gte-multilingual-reranker-base int8)")?;

        Ok(Self {
            embedder: Mutex::new(embedder),
            reranker: Mutex::new(reranker),
        })
    }

    /// Vetores de documentos (memórias).
    pub fn embed_passages(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        let prefixed: Vec<String> = texts.iter().map(|t| passage(t)).collect();
        let mut embedder = self
            .embedder
            .lock()
            .map_err(|_| anyhow!("modelo de embedding envenenado"))?;
        Ok(embedder.embed(prefixed, None)?)
    }

    /// Vetor de uma consulta (o prompt).
    pub fn embed_query(&self, text: &str) -> Result<Vec<f32>> {
        let mut embedder = self
            .embedder
            .lock()
            .map_err(|_| anyhow!("modelo de embedding envenenado"))?;
        embedder
            .embed(vec![query(text)], None)?
            .pop()
            .ok_or_else(|| anyhow!("o modelo não devolveu vetor"))
    }

    /// Reordena `docs` pela relevância para `q`: `(índice, logit)`.
    pub fn rerank(&self, q: &str, docs: &[String]) -> Result<Vec<(usize, f32)>> {
        if docs.is_empty() {
            return Ok(Vec::new());
        }
        let cortados: Vec<String> = docs
            .iter()
            .map(|d| d.chars().take(RERANK_DOC_CHARS).collect())
            .collect();
        let refs: Vec<&str> = cortados.iter().map(String::as_str).collect();
        let mut reranker = self
            .reranker
            .lock()
            .map_err(|_| anyhow!("reranker envenenado"))?;
        Ok(reranker
            .rerank(q, refs, false, None)?
            .into_iter()
            .map(|r| (r.index, r.score))
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn e5_prefixes_are_applied() {
        assert_eq!(passage("regra"), "passage: regra");
        assert_eq!(query("como faço deploy?"), "query: como faço deploy?");
    }

    #[test]
    fn models_never_land_in_the_project_folder() {
        let dir = models_dir();
        assert!(dir.ends_with("models") || dir.ends_with("orchestrator-models"));
        let cwd = std::env::current_dir().unwrap();
        assert!(!dir.starts_with(&cwd), "{}", dir.display());
    }

    #[test]
    fn every_model_file_is_pinned_to_a_commit_and_a_checksum() {
        for f in MODEL_FILES {
            assert_eq!(f.commit.len(), 40, "{}", f.path);
            assert_eq!(f.sha256.len(), 64, "{}", f.path);
            assert!(f.sha256.chars().all(|c| c.is_ascii_hexdigit()), "{}", f.path);
            assert!(f.size > 0, "{}", f.path);
            assert!(
                f.local.starts_with(EMBED_MODEL_NAME) || f.local.starts_with(RERANK_MODEL_NAME),
                "{} fora das pastas dos modelos",
                f.local
            );
        }
    }

    #[test]
    fn the_non_commercial_reranker_is_gone() {
        for f in MODEL_FILES {
            assert!(!f.repo.contains("jina"), "{}", f.repo);
        }
    }
}
