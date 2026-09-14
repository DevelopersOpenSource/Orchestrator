//! Lógica compartilhada do servidor MCP e do hook `PreToolUse`.
//!
//! - [`rpc`]: dispatch JSON-RPC 2.0 do protocolo MCP (stdio) sobre o
//!   [`orchestrator_memory::store::MemoryStore`];
//! - [`enforce`]: avaliação de regras de `security` da memória contra uma
//!   tool call (`deny-regex:` bloqueia, `ask-regex:` pausa e pergunta);
//! - [`gate`]: a decisão da trava, igual no hook de cada ferramenta de IA,
//!   no servidor MCP em modo trava e no chat HTTP.

pub mod enforce;
pub mod gate;
pub mod rpc;
pub mod ui;

use std::path::PathBuf;

use anyhow::{Context, Result};
use orchestrator_memory::store::MemoryStore;

/// Abre o `MemoryStore` no caminho padrão (ou `db_path`, se dado).
///
/// Só SQLite, nenhum modelo: o hook roda a cada evento e o MCP a cada sessão.
/// Busca semântica é pedida ao `orchestrator-memoryd`
/// (`orchestrator_memory::daemon`), com o léxico daqui como reserva.
pub fn open_store(db_path: Option<PathBuf>) -> Result<MemoryStore> {
    let path = match db_path {
        Some(p) => p,
        None => orchestrator_core::config::default_memory_db_path(),
    };
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("criando diretório {}", parent.display()))?;
    }
    MemoryStore::open(&path).with_context(|| format!("abrindo memória em {}", path.display()))
}
