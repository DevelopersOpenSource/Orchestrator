//! `orchestrator-mcp` — servidor MCP stdio que expõe a memória semântica.
//!
//! Registrado no projeto via `.mcp.json`; o agente CLI o spawna e fala
//! JSON-RPC 2.0 linha a linha por stdin/stdout. Logs vão para stderr
//! (stdout é reservado ao protocolo).

use std::io::{BufRead, Write};
use std::path::PathBuf;

use anyhow::Result;
use orchestrator_mcp_server::{open_store, rpc};

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "warn".into()),
        )
        .init();

    let db_path = std::env::var_os("ORCHESTRATOR_DB").map(PathBuf::from);
    let store = open_store(db_path)?;

    let stdin = std::io::stdin().lock();
    let mut stdout = std::io::stdout().lock();
    for line in stdin.lines() {
        let line = line?;
        if let Some(response) = rpc::handle_line(&store, &line) {
            writeln!(stdout, "{response}")?;
            stdout.flush()?;
        }
    }
    Ok(())
}
