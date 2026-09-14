//! O socket do memoryd: uma requisição JSON por linha, uma resposta por
//! linha, e fecha.

use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result};
use orchestrator_memory::daemon::{Request, Response};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};

use crate::engine::Engine;

/// Tamanho máximo do caminho de um socket unix (108 bytes com o nulo final).
pub const MAX_SOCKET_PATH: usize = 107;

/// Abre o socket. Devolve `None` se outro memoryd já responde nele.
///
/// Arquivo de socket sem ninguém escutando é sobra de um memoryd que morreu:
/// é removido e o bind segue.
pub fn bind(path: &Path) -> Result<Option<UnixListener>> {
    // O endereço de um socket unix cabe em 108 bytes (com o nulo final). Um
    // caminho maior falha com "path must be shorter than SUN_LEN", que não
    // diz o que fazer — e aconteceu de verdade com uma pasta temporária.
    if path.as_os_str().len() > MAX_SOCKET_PATH {
        anyhow::bail!(
            "o caminho do socket tem {} bytes e o sistema aceita no máximo {MAX_SOCKET_PATH}: {} \
             — defina um caminho mais curto em ORCHESTRATOR_MEMORYD_SOCKET",
            path.as_os_str().len(),
            path.display()
        );
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("criando {}", dir.display()))?;
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700)).ok();
    }
    if path.exists() {
        if std::os::unix::net::UnixStream::connect(path).is_ok() {
            return Ok(None);
        }
        std::fs::remove_file(path).ok();
    }
    Ok(Some(
        UnixListener::bind(path).with_context(|| format!("abrindo {}", path.display()))?,
    ))
}

/// Atende conexões para sempre.
pub async fn serve(engine: Arc<Engine>, listener: UnixListener) -> Result<()> {
    loop {
        let (stream, _) = listener.accept().await?;
        let engine = engine.clone();
        tokio::spawn(async move {
            if let Err(e) = atender(engine, stream).await {
                tracing::debug!("conexão encerrada com erro: {e:#}");
            }
        });
    }
}

async fn atender(engine: Arc<Engine>, stream: UnixStream) -> Result<()> {
    let (leitura, mut escrita) = stream.into_split();
    let mut linhas = BufReader::new(leitura).lines();
    if let Some(linha) = linhas.next_line().await? {
        let resposta = match serde_json::from_str::<Request>(linha.trim()) {
            Ok(pedido) => engine.handle(pedido).await,
            Err(e) => Response::failed(format!("requisição inválida: {e}")),
        };
        let mut saida = serde_json::to_string(&resposta)?;
        saida.push('\n');
        escrita.write_all(saida.as_bytes()).await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use orchestrator_memory::store::MemoryStore;
    use orchestrator_memory::MemoryKind;

    #[tokio::test]
    async fn the_socket_answers_with_the_lexical_context_while_models_load() {
        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("sub").join("memoryd.sock");
        let store = MemoryStore::open_in_memory().unwrap();
        store
            .add_memory("loja", MemoryKind::Architecture, "deploy no fly", "flyctl", 3)
            .unwrap();
        let engine = Engine::new(store);
        let listener = bind(&sock).unwrap().expect("socket livre");
        tokio::spawn(serve(engine, listener));

        // Um segundo bind no mesmo caminho percebe que já há alguém ali.
        assert!(bind(&sock).unwrap().is_none());

        let resposta = tokio::task::spawn_blocking({
            let sock = sock.clone();
            move || {
                std::env::set_var(orchestrator_memory::daemon::SOCKET_ENV, &sock);
                let r = orchestrator_memory::daemon::call(
                    &Request::Context {
                        project: "loja".into(),
                        prompt: "como faço o deploy?".into(),
                        author: "orquestrador".into(),
                    },
                    orchestrator_memory::daemon::REQUEST_TIMEOUT,
                );
                std::env::remove_var(orchestrator_memory::daemon::SOCKET_ENV);
                r
            }
        })
        .await
        .unwrap()
        .unwrap();
        assert!(!resposta.semantic);
        assert!(resposta.text.contains("deploy no fly"), "{}", resposta.text);
    }

    #[tokio::test]
    async fn a_socket_path_too_long_is_refused_with_an_explanation() {
        let longo = std::path::PathBuf::from(format!("/tmp/{}/memoryd.sock", "x".repeat(120)));
        let erro = bind(&longo).unwrap_err().to_string();
        assert!(erro.contains("ORCHESTRATOR_MEMORYD_SOCKET"), "{erro}");
        assert!(erro.contains("107"), "{erro}");
    }

    #[tokio::test]
    async fn a_stale_socket_file_is_replaced() {
        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("memoryd.sock");
        std::fs::write(&sock, b"sobra").unwrap();
        assert!(bind(&sock).unwrap().is_some());
    }
}
