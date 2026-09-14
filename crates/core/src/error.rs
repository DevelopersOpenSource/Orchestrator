//! Erros comuns do núcleo do Orchestrator.

use std::path::PathBuf;

/// Erros produzidos pelo crate `orchestrator-core`.
#[derive(Debug, thiserror::Error)]
pub enum CoreError {
    /// Não foi possível determinar os diretórios padrão do usuário
    /// (config/data) via a crate `directories`.
    #[error("não foi possível determinar os diretórios padrão do usuário")]
    NoProjectDirs,

    /// Falha de IO ao ler/gravar um arquivo de configuração.
    #[error("erro de IO em {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    /// O arquivo de configuração existe mas não é um JSON válido.
    #[error("configuração inválida em {path}: {source}")]
    InvalidConfig {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },

    /// Projeto referenciado não existe na configuração.
    #[error("projeto desconhecido: {0}")]
    UnknownProject(String),

    /// Transição de estado de sessão não permitida.
    #[error("transição de estado inválida: {from} -> {to}")]
    InvalidTransition { from: String, to: String },
}
