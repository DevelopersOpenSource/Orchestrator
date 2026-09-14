//! `orchestrator` — CLI do Orchestrator.
//!
//! NOTA: por enquanto os comandos chamam as bibliotecas diretamente no
//! mesmo processo (memória e adapter). A comunicação com o serviço de
//! fundo (`orchestrator-service`) via IPC virá depois; quando existir,
//! estes comandos passarão a ser meros clientes do serviço.

mod facade;

use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use orchestrator_core::{config, Config};

/// CLI do Orchestrator — segundo cérebro de contexto para agentes CLI.
#[derive(Debug, Parser)]
#[command(name = "orchestrator", version, about)]
struct Args {
    /// Caminho do arquivo de configuração (JSON).
    #[arg(long)]
    config: Option<PathBuf>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Operações sobre a memória semântica.
    Memory {
        #[command(subcommand)]
        command: MemoryCommand,
    },
    /// Lista as decisões registradas de um projeto.
    Decisions {
        #[arg(long)]
        project: String,
    },
    /// Roda uma sessão one-shot do agente em um projeto.
    Run {
        #[arg(long)]
        project: String,
        #[arg(long)]
        prompt: String,
    },
    /// Abre a interface (dashboard, fila de decisões, memórias, auditoria).
    Tui,
    /// Instala hooks + servidor MCP do Orchestrator em um projeto.
    Setup {
        #[arg(long)]
        project: String,
    },
}

#[derive(Debug, Subcommand)]
enum MemoryCommand {
    /// Adiciona uma memória do DONO: no projeto (`--project`) ou geral de
    /// desenvolvimento, valendo em todo projeto (`--global`).
    Add {
        #[arg(long, conflicts_with = "global")]
        project: Option<String>,
        /// Memória geral de desenvolvimento: vale em todo projeto.
        #[arg(long)]
        global: bool,
        /// security | architecture | practice | syntax | decision
        #[arg(long)]
        kind: String,
        #[arg(long)]
        title: String,
        #[arg(long)]
        body: String,
        /// 9 ou mais (fora de security) = regra FIXA, em todo prompt.
        #[arg(long, default_value_t = 0)]
        priority: i64,
    },
    /// Busca na memória que o projeto enxerga (dele + globais).
    Search {
        #[arg(long)]
        project: String,
        #[arg(long)]
        query: String,
    },
    /// Lista memórias: as que um projeto enxerga, ou só as globais.
    List {
        #[arg(long, conflicts_with = "global")]
        project: Option<String>,
        #[arg(long)]
        global: bool,
    },
    /// Reconstrói o índice vetorial (ChromaDB) a partir do SQLite.
    Reindex,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let config_path = match &args.config {
        Some(p) => p.clone(),
        None => config::default_config_path()?,
    };
    let cfg = Config::load(&config_path)
        .with_context(|| format!("falha ao carregar configuração de {}", config_path.display()))?;

    match args.command {
        Command::Memory { command } => {
            let store = facade::open_memory(&cfg.memory_db_path)?;
            match command {
                MemoryCommand::Add {
                    project,
                    global,
                    kind,
                    title,
                    body,
                    priority,
                } => facade::add_memory(
                    &store,
                    project.as_deref(),
                    global,
                    &kind,
                    &title,
                    &body,
                    priority,
                ),
                MemoryCommand::Search { project, query } => {
                    facade::search_memory(&store, &project, &query)
                }
                MemoryCommand::List { project, global } => {
                    facade::list_memories(&store, project.as_deref(), global)
                }
                MemoryCommand::Reindex => facade::reindex_memory(),
            }
        }
        Command::Decisions { project } => {
            let store = facade::open_memory(&cfg.memory_db_path)?;
            facade::list_decisions(&store, &project)
        }
        Command::Run { project, prompt } => {
            let project_cfg = cfg.project(&project)?;
            facade::run_agent_turn(&project_cfg.path, &prompt).await
        }
        Command::Tui => {
            // A TUI bloqueia o terminal; roda fora do runtime async.
            tokio::task::block_in_place(|| orchestrator_tui::run(&cfg, None, Some(config_path.clone())))
        }
        Command::Setup { project } => {
            let project_cfg = cfg.project(&project)?;
            facade::setup_project(&project_cfg.path, &project)
        }
    }
}
