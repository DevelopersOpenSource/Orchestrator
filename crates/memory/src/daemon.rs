//! Protocolo e cliente do `orchestrator-memoryd`.
//!
//! O memoryd é o único processo que carrega modelos (embedding e reranker) e
//! fala com o ChromaDB. Quem precisa de busca semântica — o hook
//! `UserPromptSubmit`, o servidor MCP, a CLI, a TUI — conversa com ele por um
//! socket unix, uma requisição JSON por linha.
//!
//! Tudo aqui é std e sem dependência pesada, porque roda dentro do hook. E
//! toda chamada tem prazo: se o memoryd não responder, quem chamou cai no
//! ranqueamento léxico do SQLite e o prompt segue.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::Duration;

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

/// Variável que troca o caminho do socket (testes e instalações especiais).
pub const SOCKET_ENV: &str = "ORCHESTRATOR_MEMORYD_SOCKET";

/// `0` desliga a subida automática do memoryd — testes (o binário testado
/// fica ao lado do memoryd e dispararia modelos de verdade) e ambientes que
/// não querem modelo nenhum. Ausente ou qualquer outro valor: ligada.
pub const AUTOSTART_ENV: &str = "ORCHESTRATOR_MEMORYD_AUTOSTART";

/// Nome do binário, procurado ao lado do executável atual.
pub const BINARY: &str = "orchestrator-memoryd";

/// Prazo curto para saber se o memoryd está vivo.
pub const HEALTH_TIMEOUT: Duration = Duration::from_millis(300);

/// Prazo de uma requisição de contexto ou busca (modelos já carregados).
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(8);

/// Uma requisição ao memoryd.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Request {
    /// Está vivo? (e com Chroma?)
    Health,
    /// O texto invisível completo para um prompt.
    Context {
        project: String,
        prompt: String,
        author: String,
    },
    /// Busca ranqueada (projeto + global).
    Search {
        project: String,
        query: String,
        top_k: usize,
        kind: Option<String>,
    },
    /// (Re)indexar estas memórias depois de gravar ou editar.
    Index { ids: Vec<String> },
    /// Tirar estas memórias do índice depois de apagar.
    Remove { ids: Vec<String> },
    /// Reconstruir o índice inteiro a partir do SQLite.
    Reindex,
}

/// Um resultado de busca.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Hit {
    pub id: String,
    pub score: f32,
}

/// A resposta do memoryd.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Response {
    pub ok: bool,
    /// Texto pronto (contexto) ou mensagem.
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub hits: Vec<Hit>,
    /// A busca passou pelo ChromaDB + reranker?
    #[serde(default)]
    pub semantic: bool,
    #[serde(default)]
    pub error: Option<String>,
}

impl Response {
    pub fn ok_text(text: impl Into<String>, semantic: bool) -> Self {
        Self {
            ok: true,
            text: text.into(),
            semantic,
            ..Self::default()
        }
    }

    pub fn failed(error: impl Into<String>) -> Self {
        Self {
            ok: false,
            error: Some(error.into()),
            ..Self::default()
        }
    }
}

/// Onde o socket fica: `$ORCHESTRATOR_MEMORYD_SOCKET`, senão
/// `$XDG_RUNTIME_DIR/orchestrator/memoryd.sock`, senão a pasta temporária.
pub fn socket_path() -> PathBuf {
    socket_path_from(
        std::env::var_os(SOCKET_ENV).map(PathBuf::from),
        std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from),
    )
}

fn socket_path_from(explicit: Option<PathBuf>, runtime: Option<PathBuf>) -> PathBuf {
    if let Some(p) = explicit.filter(|p| !p.as_os_str().is_empty()) {
        return p;
    }
    match runtime.filter(|p| !p.as_os_str().is_empty()) {
        Some(dir) => dir.join("orchestrator").join("memoryd.sock"),
        None => std::env::temp_dir().join("orchestrator-memoryd.sock"),
    }
}

/// Arquivo com o PID do memoryd — encerrar só por ele, nunca por nome.
pub fn pid_path() -> PathBuf {
    socket_path().with_extension("pid")
}

/// Log do memoryd (a saída dele não tem terminal).
pub fn log_path() -> PathBuf {
    orchestrator_core::config::default_memory_db_path()
        .parent()
        .map(|d| d.join("memoryd.log"))
        .unwrap_or_else(|| std::env::temp_dir().join("orchestrator-memoryd.log"))
}

/// Envia uma requisição e espera a resposta, com prazo.
pub fn call(request: &Request, timeout: Duration) -> Result<Response> {
    let path = socket_path();
    let stream = UnixStream::connect(&path)
        .with_context(|| format!("memoryd não responde em {}", path.display()))?;
    stream.set_read_timeout(Some(timeout))?;
    stream.set_write_timeout(Some(timeout))?;
    let mut line = serde_json::to_string(request)?;
    line.push('\n');
    (&stream).write_all(line.as_bytes())?;
    let mut reader = BufReader::new(stream);
    let mut out = String::new();
    reader
        .read_line(&mut out)
        .context("memoryd não respondeu a tempo")?;
    if out.trim().is_empty() {
        bail!("memoryd fechou a conexão sem responder");
    }
    let response: Response = serde_json::from_str(out.trim())
        .with_context(|| format!("resposta ilegível do memoryd: {}", out.trim()))?;
    if !response.ok {
        bail!(
            "{}",
            response
                .error
                .unwrap_or_else(|| "memoryd recusou a requisição".to_string())
        );
    }
    Ok(response)
}

/// O memoryd está vivo?
pub fn is_running() -> bool {
    call(&Request::Health, HEALTH_TIMEOUT).is_ok()
}

/// Caminho do binário do memoryd, ao lado do executável atual.
pub fn binary_path() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let bin = exe.parent()?.join(BINARY);
    bin.is_file().then_some(bin)
}

/// Sobe o memoryd em segundo plano, sem esperar ele ficar pronto.
///
/// Devolve `false` se o binário não existe. O próprio memoryd sai da sessão
/// de quem o chamou (`setsid`) — senão fechar a CLI que o disparou pelo hook
/// o levaria junto — e encerra sozinho depois de um tempo ocioso.
pub fn spawn_detached() -> Result<bool> {
    if std::env::var(AUTOSTART_ENV).as_deref() == Ok("0") {
        return Ok(false);
    }
    let Some(bin) = binary_path() else {
        return Ok(false);
    };
    let log = log_path();
    if let Some(dir) = log.parent() {
        std::fs::create_dir_all(dir).ok();
    }
    let stderr = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log)
        .map(Stdio::from)
        .unwrap_or_else(|_| Stdio::null());
    let mut child = Command::new(bin)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(stderr)
        .spawn()
        .context("subindo o orchestrator-memoryd")?;
    // Colhe o status para não deixar zumbi num processo de vida longa (TUI).
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(true)
}

/// Garante que o memoryd esteja subindo: se não responde, dispara e segue.
///
/// Devolve `true` só quando ele JÁ respondia — quem chamou não deve esperar o
/// carregamento dos modelos (segundos, e minutos no primeiro download).
pub fn ensure_started() -> bool {
    if is_running() {
        return true;
    }
    let _ = spawn_detached();
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::net::UnixListener;

    #[test]
    fn requests_travel_as_one_tagged_json_line() {
        let req = Request::Context {
            project: "loja".into(),
            prompt: "como faço deploy?".into(),
            author: "frontend".into(),
        };
        let json = serde_json::to_string(&req).unwrap();
        assert!(json.contains("\"op\":\"context\""), "{json}");
        assert!(!json.contains('\n'));
        assert_eq!(serde_json::from_str::<Request>(&json).unwrap(), req);
        let health = serde_json::to_string(&Request::Health).unwrap();
        assert_eq!(health, "{\"op\":\"health\"}");
    }

    #[test]
    fn socket_path_prefers_the_explicit_then_the_runtime_dir() {
        assert_eq!(
            socket_path_from(Some("/x/y.sock".into()), Some("/run/user/1000".into())),
            PathBuf::from("/x/y.sock")
        );
        assert_eq!(
            socket_path_from(None, Some("/run/user/1000".into())),
            PathBuf::from("/run/user/1000/orchestrator/memoryd.sock")
        );
        assert!(socket_path_from(Some("".into()), None)
            .to_string_lossy()
            .ends_with("orchestrator-memoryd.sock"));
    }

    #[test]
    fn autostart_can_be_switched_off() {
        std::env::set_var(AUTOSTART_ENV, "0");
        assert!(!spawn_detached().unwrap(), "com a chave em 0 nada pode subir");
        std::env::remove_var(AUTOSTART_ENV);
    }

    #[test]
    fn a_missing_daemon_fails_fast_instead_of_hanging() {
        std::env::set_var(SOCKET_ENV, "/tmp/nao-existe-orchestrator-memoryd.sock");
        let inicio = std::time::Instant::now();
        assert!(call(&Request::Health, HEALTH_TIMEOUT).is_err());
        assert!(inicio.elapsed() < Duration::from_secs(1));
        std::env::remove_var(SOCKET_ENV);
    }

    #[test]
    fn a_real_socket_round_trip_works() {
        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("m.sock");
        let listener = UnixListener::bind(&sock).unwrap();
        let servidor = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut linha = String::new();
            BufReader::new(&stream).read_line(&mut linha).unwrap();
            let pedido: Request = serde_json::from_str(linha.trim()).unwrap();
            let resposta = match pedido {
                Request::Context { project, .. } => Response::ok_text(format!("ctx de {project}"), true),
                _ => Response::failed("inesperado"),
            };
            let mut out = serde_json::to_string(&resposta).unwrap();
            out.push('\n');
            (&stream).write_all(out.as_bytes()).unwrap();
        });
        // Este teste troca a variável global; o de "daemon ausente" usa outro
        // valor, e cada um restaura ao sair.
        let antigo = std::env::var_os(SOCKET_ENV);
        std::env::set_var(SOCKET_ENV, &sock);
        let r = call(
            &Request::Context {
                project: "loja".into(),
                prompt: "x".into(),
                author: "y".into(),
            },
            REQUEST_TIMEOUT,
        );
        match antigo {
            Some(v) => std::env::set_var(SOCKET_ENV, v),
            None => std::env::remove_var(SOCKET_ENV),
        }
        servidor.join().unwrap();
        let r = r.unwrap();
        assert!(r.semantic);
        assert_eq!(r.text, "ctx de loja");
    }
}
