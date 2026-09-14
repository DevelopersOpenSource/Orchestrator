//! Trait genérica para agentes de código CLI controlados por subprocesso.

use std::path::PathBuf;

use async_trait::async_trait;
use tokio::sync::mpsc;

use crate::claude_code::stream::StreamEvent;

/// Erros da camada de adaptação de agentes CLI.
#[derive(Debug, thiserror::Error)]
pub enum AgentError {
    /// Falha ao iniciar o subprocesso do agente.
    #[error("falha ao iniciar o processo do agente: {0}")]
    Spawn(#[source] std::io::Error),

    /// O processo não expôs stdout/stdin como esperado.
    #[error("stdio do processo do agente indisponível: {0}")]
    Stdio(String),

    /// O agente terminou sem produzir um resultado final.
    #[error("o agente terminou sem evento de resultado")]
    NoResult,

    /// Erro de E/S genérico.
    #[error("erro de E/S: {0}")]
    Io(#[from] std::io::Error),
}

/// Identifica uma sessão em andamento de um agente CLI.
///
/// O `session_id` é atribuído pelo próprio agente (evento `system/init`)
/// e é usado para retomar a conversa em turnos subsequentes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionHandle {
    /// Identificador da sessão emitido pelo agente.
    pub session_id: String,
    /// Diretório do projeto sobre o qual a sessão opera.
    pub project_dir: PathBuf,
    /// Modelo reportado pelo agente, quando conhecido.
    pub model: Option<String>,
}

/// Um agente de código CLI controlável programaticamente.
///
/// Implementações DEVEM usar apenas subprocesso + stdio JSON —
/// nunca automação de teclado.
#[async_trait]
pub trait CliAgent: Send + Sync {
    /// Nome legível do agente (ex.: `"claude-code"`).
    fn name(&self) -> &str;

    /// Indica se o agente suporta hooks de projeto (`.claude/settings.json`).
    fn supports_hooks(&self) -> bool {
        false
    }

    /// Indica se o agente suporta servidores MCP (`.mcp.json`).
    fn supports_mcp(&self) -> bool {
        false
    }

    /// Inicia uma nova sessão no diretório do projeto com o prompt inicial.
    ///
    /// Retorna o handle da sessão e o stream de eventos do primeiro turno.
    async fn start_session(
        &self,
        project_dir: &std::path::Path,
        prompt: &str,
    ) -> Result<(SessionHandle, mpsc::Receiver<StreamEvent>), AgentError>;

    /// Envia um novo turno para uma sessão existente e retorna o stream
    /// de eventos produzidos por ele.
    async fn send_turn(
        &self,
        session: &SessionHandle,
        prompt: &str,
    ) -> Result<mpsc::Receiver<StreamEvent>, AgentError>;
}
