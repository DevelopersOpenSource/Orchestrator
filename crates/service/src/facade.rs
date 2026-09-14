//! Fachada fina sobre `orchestrator-memory` e `orchestrator-cli-adapter`.
//!
//! Esses crates são implementados em paralelo; concentramos aqui todas as
//! chamadas às suas APIs públicas para que um eventual descasamento de
//! assinatura fique localizado neste arquivo.

use std::path::Path;

use anyhow::{Context, Result};
use orchestrator_cli_adapter::{ClaudeCodeAgent, StreamEvent};
use orchestrator_memory::store::MemoryStore;

/// Alias local para o armazenamento de memória.
pub type Memory = MemoryStore;

/// Abre (criando se preciso) o banco de memória em `path`.
///
pub fn open_memory(path: &Path) -> Result<Memory> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("criando diretório {}", parent.display()))?;
    }
    MemoryStore::open(path)
        .with_context(|| format!("falha ao abrir memória em {}", path.display()))
}

/// Roda um turno one-shot do agente Claude Code em `workdir`, entregando
/// cada `StreamEvent` ao callback `on_event`.
pub async fn run_agent_turn(
    workdir: &Path,
    prompt: &str,
    mut on_event: impl FnMut(&StreamEvent),
) -> Result<()> {
    let agent = ClaudeCodeAgent::default();
    let mut events = agent
        .run_turn(workdir, prompt, None)
        .await
        .context("falha ao iniciar turno do agente")?;
    while let Some(event) = events.recv().await {
        on_event(&event);
    }
    Ok(())
}
