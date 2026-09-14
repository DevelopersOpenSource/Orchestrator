//! Entrada do `orchestrator-memoryd`.
//!
//! Sobe o socket na hora (o caminho léxico já responde), carrega os modelos
//! e conecta no ChromaDB em segundo plano, indexa o que estiver pendente a
//! cada 30 segundos e sai sozinho após 30 minutos sem ninguém pedir nada.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use orchestrator_memory::daemon;
use orchestrator_memory::store::MemoryStore;
use orchestrator_memoryd::chroma;
use orchestrator_memoryd::engine::Engine;
use orchestrator_memoryd::http;
use orchestrator_memoryd::download;
use orchestrator_memoryd::models::{models_dir, Models, MODEL_FILES};
use orchestrator_memoryd::server;
use tokio::signal::unix::{signal, SignalKind};

/// Sem requisição por este tempo, o memoryd encerra (quem precisar, sobe de
/// novo — o hook dispara sozinho).
const IDLE_EXIT: Duration = Duration::from_secs(30 * 60);

/// Intervalo da indexação periódica (pega o que foi gravado sem aviso).
const INDEX_EVERY: Duration = Duration::from_secs(30);

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info".into()),
        )
        .init();

    // Sai da sessão de quem nos chamou. O memoryd costuma nascer pelo hook de
    // uma CLI, e fechar o card da CLI encerra a SESSÃO dela inteira — sem
    // isto o serviço de memória morreria junto. Não vira órfão eterno: ele
    // encerra sozinho quando fica ocioso.
    unsafe {
        libc::setsid();
    }

    let socket = daemon::socket_path();
    let Some(listener) = server::bind(&socket)? else {
        tracing::info!("já há um memoryd respondendo em {}", socket.display());
        return Ok(());
    };
    let pid_file = daemon::pid_path();
    std::fs::write(&pid_file, std::process::id().to_string())
        .with_context(|| format!("gravando {}", pid_file.display()))?;

    let db = std::env::var_os("ORCHESTRATOR_DB")
        .map(PathBuf::from)
        .unwrap_or_else(orchestrator_core::config::default_memory_db_path);
    let store = MemoryStore::open(&db).with_context(|| format!("abrindo {}", db.display()))?;
    let engine = Engine::new(store);
    tracing::info!(
        "memoryd pid {} em {} (banco {})",
        std::process::id(),
        socket.display(),
        db.display()
    );

    carregar_modelos(engine.clone());
    manter_indice(engine.clone());
    iniciar_api(engine.clone());

    let mut termino = signal(SignalKind::terminate())?;
    let mut interrupcao = signal(SignalKind::interrupt())?;
    tokio::select! {
        r = server::serve(engine.clone(), listener) => { r?; }
        _ = ocioso(engine.clone()) => tracing::info!("ocioso há {} min — encerrando", IDLE_EXIT.as_secs() / 60),
        _ = termino.recv() => tracing::info!("SIGTERM — encerrando"),
        _ = interrupcao.recv() => tracing::info!("SIGINT — encerrando"),
    }

    // Só remove o socket se ainda for o nosso (outro memoryd pode tê-lo
    // assumido depois de um bind com sobra).
    if std::fs::read_to_string(&pid_file).ok().as_deref() == Some(&std::process::id().to_string()) {
        std::fs::remove_file(&pid_file).ok();
        std::fs::remove_file(&socket).ok();
        std::fs::remove_file(http::port_file()).ok();
    }
    Ok(())
}

fn carregar_modelos(engine: Arc<Engine>) {
    tokio::spawn(async move {
        let inicio = std::time::Instant::now();
        let base = models_dir();
        // 1ª execução: baixa com checksum (o health mostra o %). Sem rede,
        // o serviço segue pelo caminho léxico e tenta de novo na próxima vez.
        if let Err(e) = download::ensure(&base, &MODEL_FILES, &engine.download).await {
            tracing::error!("modelos não baixaram (sigo léxico): {e:#}");
            return;
        }
        match tokio::task::spawn_blocking(move || Models::load(&base)).await {
            Ok(Ok(models)) => {
                engine.set_models(models);
                tracing::info!("modelos prontos em {:.1}s", inicio.elapsed().as_secs_f32());
                if let Err(e) = engine.index_pending().await {
                    tracing::warn!("indexação inicial falhou: {e:#}");
                }
            }
            Ok(Err(e)) => tracing::error!("modelos não carregaram (sigo léxico): {e:#}"),
            Err(e) => tracing::error!("carga de modelos abortou: {e}"),
        }
    });
}

/// A memória como API + página no loopback (leitura livre, escrita com token).
fn iniciar_api(engine: Arc<Engine>) {
    tokio::spawn(async move {
        let token = match http::load_or_create_token(&http::token_path()) {
            Ok(t) => t,
            Err(e) => {
                tracing::error!("API da memória não sobe sem token: {e:#}");
                return;
            }
        };
        match http::bind().await {
            Ok((listener, porta)) => {
                tracing::info!("API da memória em http://127.0.0.1:{porta}");
                if let Err(e) = http::serve(engine, listener, porta, token).await {
                    tracing::error!("API da memória parou: {e:#}");
                }
            }
            Err(e) => tracing::warn!("API da memória não subiu: {e:#}"),
        }
    });
}

fn manter_indice(engine: Arc<Engine>) {
    tokio::spawn(async move {
        loop {
            if !engine.has_collection().await {
                match chroma::connect().await {
                    Ok(col) => {
                        engine.set_collection(col).await;
                        tracing::info!("ChromaDB conectado");
                    }
                    Err(e) => tracing::warn!("ChromaDB indisponível (tento de novo): {e:#}"),
                }
            }
            if let Err(e) = engine.index_pending().await {
                tracing::warn!("indexação periódica falhou (reconecto na próxima volta): {e:#}");
                // O container pode ter sido recriado sem os dados — a coleção
                // ganha outro id e o antigo não serve mais. Reconectar resolve.
                engine.clear_collection().await;
            }
            tokio::time::sleep(INDEX_EVERY).await;
        }
    });
}

async fn ocioso(engine: Arc<Engine>) {
    loop {
        tokio::time::sleep(Duration::from_secs(60)).await;
        if engine.idle_for() >= IDLE_EXIT {
            return;
        }
    }
}
