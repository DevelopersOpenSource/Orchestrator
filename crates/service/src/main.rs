//! `orchestrator-service` — serviço de fundo do Orchestrator.
//!
//! Esqueleto real, porém mínimo: carrega a configuração, abre o
//! `MemoryStore` e mantém um laço de execução capaz de iniciar sessões
//! de agente via `orchestrator-cli-adapter`. O binário `claude` só é
//! necessário quando uma sessão de fato inicia.

mod facade;

use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::Parser;
use orchestrator_core::{config, Config, Project, SessionState};
use tracing::{error, info};

/// Serviço de fundo do Orchestrator.
#[derive(Debug, Parser)]
#[command(name = "orchestrator-service", version, about)]
struct Args {
    /// Caminho do arquivo de configuração (JSON). Padrão:
    /// `~/.config/orchestrator/config.json`.
    #[arg(long)]
    config: Option<PathBuf>,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info".into()),
        )
        .init();

    let args = Args::parse();
    let config_path = match args.config {
        Some(p) => p,
        None => config::default_config_path()?,
    };
    let config = Config::load(&config_path)
        .with_context(|| format!("falha ao carregar configuração de {}", config_path.display()))?;
    info!(
        config = %config_path.display(),
        projects = config.projects.len(),
        "orchestrator-service iniciando"
    );

    let store = facade::open_memory(&config.memory_db_path)?;
    info!(db = %config.memory_db_path.display(), "MemoryStore aberto");

    let mut projects: Vec<Project> = config
        .projects
        .iter()
        .cloned()
        .map(Project::new)
        .collect();

    run_loop(&config, &store, &mut projects).await
}

/// Laço principal do serviço.
///
/// Por enquanto o serviço não recebe comandos externos (o IPC com o CLI
/// virá depois); ele apenas fica vivo até ctrl+c. `start_session` mostra
/// o caminho completo de uma sessão e é chamado sob demanda no futuro.
async fn run_loop(
    config: &Config,
    store: &facade::Memory,
    projects: &mut [Project],
) -> Result<()> {
    let _ = (config, store); // usados quando sessões forem disparadas por IPC

    info!("laço principal ativo; aguardando ctrl+c");
    tokio::signal::ctrl_c()
        .await
        .context("falha ao instalar handler de ctrl+c")?;

    for p in projects.iter().filter(|p| p.state == SessionState::Running) {
        info!(project = %p.config.name, "sessão ainda em execução no desligamento");
    }
    info!("ctrl+c recebido; encerrando com graça");
    Ok(())
}

/// Inicia uma sessão one-shot em um projeto e registra os `StreamEvent`s
/// via `tracing`. Requer o binário do agente (ex.: `claude`) instalado —
/// só é chamado quando uma sessão inicia, nunca na subida do serviço.
#[allow(dead_code)]
async fn start_session(project: &mut Project, prompt: &str) -> Result<()> {
    project.transition(SessionState::Running)?;
    info!(project = %project.config.name, "sessão iniciada");

    let result = facade::run_agent_turn(&project.config.path, prompt, |event| {
        info!(project = %project.config.name, ?event, "stream");
    })
    .await;

    match result {
        Ok(()) => {
            project.transition(SessionState::Done)?;
            info!(project = %project.config.name, "sessão concluída");
            Ok(())
        }
        Err(err) => {
            error!(project = %project.config.name, error = %err, "sessão falhou");
            project.transition(SessionState::Failed)?;
            Err(err)
        }
    }
}
