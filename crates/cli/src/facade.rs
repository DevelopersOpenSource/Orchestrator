//! Fachada fina sobre `orchestrator-memory` e `orchestrator-cli-adapter`.
//!
//! As APIs desses crates são implementadas em paralelo por outros agentes;
//! toda chamada a elas passa por aqui para localizar qualquer descasamento
//! de assinatura em um único arquivo.

use std::path::Path;
use std::str::FromStr;

use anyhow::{Context, Result};
use orchestrator_cli_adapter::{ClaudeCodeAgent, StreamEvent};
use orchestrator_memory::store::MemoryStore;
use orchestrator_memory::MemoryKind;

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

/// Uma memória em uma linha de cabeçalho + corpo recuado.
fn print_memory(m: &orchestrator_memory::Memory, score: Option<f32>) {
    let score = score.map(|s| format!(" · score {s:.3}")).unwrap_or_default();
    println!(
        "[{} · {} · p{}{score}] {}  ({})",
        m.origin_label(),
        m.kind,
        m.priority,
        m.title,
        m.id
    );
    for linha in m.body.lines() {
        println!("    {linha}");
    }
}

/// Adiciona uma memória do DONO — no projeto ou global — e pede ao memoryd
/// para indexá-la (se ele não responder, a indexação periódica pega depois).
pub fn add_memory(
    store: &Memory,
    project: Option<&str>,
    global: bool,
    kind: &str,
    title: &str,
    body: &str,
    priority: i64,
) -> Result<()> {
    use orchestrator_memory::store::NewMemory;
    let kind = MemoryKind::from_str(kind)?;
    let nova = match (global, project) {
        (true, _) => NewMemory::global(kind, title, body, priority),
        (false, Some(p)) => NewMemory::user(p, kind, title, body, priority),
        (false, None) => anyhow::bail!("informe --project <nome> ou --global"),
    };
    let record = store.add(nova).context("falha ao adicionar memória")?;
    let _ = orchestrator_memory::daemon::call(
        &orchestrator_memory::daemon::Request::Index {
            ids: vec![record.id.clone()],
        },
        orchestrator_memory::daemon::HEALTH_TIMEOUT,
    );
    println!("memória gravada:");
    print_memory(&record, None);
    Ok(())
}

/// Busca pelo memoryd (semântica); sem ele, léxica — e diz qual foi.
pub fn search_memory(store: &Memory, project: &str, query: &str) -> Result<()> {
    use orchestrator_memory::daemon::{self, Request};
    let pedido = Request::Search {
        project: project.to_string(),
        query: query.to_string(),
        top_k: 10,
        kind: None,
    };
    match daemon::call(&pedido, daemon::REQUEST_TIMEOUT) {
        Ok(resposta) => {
            println!(
                "{}",
                if resposta.semantic {
                    "busca semântica (ChromaDB + reranker)"
                } else {
                    "busca por palavras-chave (memoryd ainda sem modelos ou sem Chroma)"
                }
            );
            for hit in resposta.hits {
                if let Some(m) = store.get(&hit.id)? {
                    print_memory(&m, Some(hit.score));
                }
            }
        }
        Err(_) => {
            println!("busca por palavras-chave (o memoryd não respondeu)");
            for r in store
                .search(project, query, 10, None)
                .context("falha na busca de memória")?
            {
                print_memory(&r.memory, Some(r.score));
            }
        }
    }
    Ok(())
}

/// Lista o que um projeto enxerga (dele + globais), ou só as globais.
pub fn list_memories(store: &Memory, project: Option<&str>, global: bool) -> Result<()> {
    let lista = match (global, project) {
        (true, _) => store.list(orchestrator_memory::GLOBAL_PROJECT, None)?,
        (false, Some(p)) => store.list_visible(p, None)?,
        (false, None) => anyhow::bail!("informe --project <nome> ou --global"),
    };
    if lista.is_empty() {
        println!("nenhuma memória");
    }
    for m in &lista {
        print_memory(m, None);
    }
    Ok(())
}

/// Reconstrói o índice vetorial pelo memoryd.
pub fn reindex_memory() -> Result<()> {
    use orchestrator_memory::daemon::{self, Request};
    if !daemon::is_running() {
        let subiu = daemon::spawn_detached().unwrap_or(false);
        anyhow::bail!(
            "o memoryd não está rodando{} — tente de novo quando os modelos carregarem",
            if subiu { " (acabei de iniciá-lo)" } else { "" }
        );
    }
    let resposta = daemon::call(&Request::Reindex, std::time::Duration::from_secs(600))?;
    println!("{}", resposta.text);
    Ok(())
}

/// Lista as decisões registradas de um projeto.
pub fn list_decisions(store: &Memory, project: &str) -> Result<()> {
    let results = store
        .list_decisions(project)
        .context("falha ao listar decisões")?;
    for r in results {
        println!("{r:#?}");
    }
    Ok(())
}

/// Roda um turno one-shot do agente Claude Code em `workdir`, imprimindo
/// cada evento do stream conforme chega.
pub async fn run_agent_turn(workdir: &Path, prompt: &str) -> Result<()> {
    let agent = ClaudeCodeAgent::default();
    let mut events = agent
        .run_turn(workdir, prompt, None)
        .await
        .context("falha ao iniciar turno do agente")?;
    while let Some(event) = events.recv().await {
        print_event(&event);
    }
    Ok(())
}

fn print_event(event: &StreamEvent) {
    println!("{event:?}");
}

/// Instala hooks (`.claude/settings.json`), servidor MCP (`.mcp.json`) e o
/// arquivo de regras do Orchestrator em um diretório de projeto.
///
/// Os binários `orchestrator-hook`/`orchestrator-mcp` são procurados ao
/// lado do executável atual (mesmo target dir / mesmo prefixo instalado).
pub fn setup_project(project_dir: &Path, project_name: &str) -> Result<()> {
    use orchestrator_cli_adapter::claude_code::hooks;

    let out = hooks::setup_project(project_dir, project_name)?;
    println!(
        "setup concluído em {}:\n  regras: {}\n  hooks (.claude/settings.json): {}\n  mcp (.mcp.json): {}",
        project_dir.display(),
        out.rules_file.display(),
        if out.hooks_written { "atualizado" } else { "já em dia" },
        if out.mcp_written { "atualizado" } else { "já em dia" },
    );
    if !out.binaries_present {
        println!(
            "aviso: binários orchestrator-hook/orchestrator-mcp não encontrados ao lado de {} — rode `cargo build --release` e/ou instale o pacote.",
            out.hook_bin.display()
        );
    }
    Ok(())
}
