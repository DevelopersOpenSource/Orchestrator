//! O que é comum às ferramentas de IA que rodam o chat do orquestrador sem
//! janela: Codex, Kimi Code, Antigravity e OpenCode.
//!
//! Cada uma tem seu comando e seu formato de saída ([`LineParser`]); o resto
//! é igual: sobe o processo na pasta do projeto, lê o stdout linha a linha,
//! traduz para [`HarnessEvent`] e avisa o fim com [`TurnEnd`].
//!
//! O Claude Code segue no adaptador próprio (`claude_code`), que é mais
//! antigo e tem streaming token a token.

use std::path::{Path, PathBuf};
use std::process::Stdio;

use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tokio::sync::mpsc;

use crate::agent::AgentError;

/// O que a ferramenta disse, já sem dialeto.
#[derive(Debug, Clone, PartialEq)]
pub enum HarnessEvent {
    /// Pedaço de texto do assistente (streaming).
    TextDelta(String),
    /// Bloco de texto completo do assistente.
    Text(String),
    /// Raciocínio exposto pela ferramenta.
    Thinking(String),
    /// Uma chamada de ferramenta, numa linha legível.
    Tool(String),
    /// Tokens do turno até agora (cumulativos).
    Tokens {
        input: u64,
        output: u64,
        cost: Option<f64>,
    },
    /// Sessão para retomar no próximo turno.
    Session(String),
    /// Erro relatado pela ferramenta.
    Error(String),
}

/// Fim do processo.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TurnEnd {
    pub success: bool,
    /// As últimas linhas do stderr, para explicar uma falha muda.
    pub stderr_tail: String,
}

#[derive(Debug, Clone, PartialEq)]
pub enum HarnessMessage {
    Event(HarnessEvent),
    End(TurnEnd),
}

/// O servidor MCP do Orchestrator, como a ferramenta deve subi-lo.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct McpLaunch {
    pub command: PathBuf,
    /// Ambiente do servidor. As ferramentas não repassam o delas inteiro,
    /// então banco, projeto e trava vão explícitos.
    pub env: Vec<(String, String)>,
}

/// Um turno do chat.
#[derive(Debug, Clone, Default)]
pub struct TurnRequest {
    /// Binário da ferramenta (caminho completo quando achado).
    pub binary: String,
    /// Pasta do projeto (a ferramenta roda nela).
    pub dir: PathBuf,
    /// A mensagem, já com o contexto do Orchestrator.
    pub prompt: String,
    /// Instruções do papel de maestro.
    pub system: String,
    pub session: Option<String>,
    pub model: Option<String>,
    /// Variáveis extras do processo (chaves, endpoints, `ORCHESTRATOR_*`).
    pub env: Vec<(String, String)>,
    pub mcp: Option<McpLaunch>,
    /// Pasta para arquivos de apoio que a ferramenta precise ler (ex.: o
    /// agente do Kimi).
    pub state_dir: PathBuf,
}

/// Comando pronto para rodar.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandSpec {
    pub program: String,
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
    pub dir: PathBuf,
}

/// Tradutor da saída de uma ferramenta.
pub trait LineParser: Send + 'static {
    /// Uma linha do stdout.
    fn feed(&mut self, line: &str) -> Vec<HarnessEvent>;
    /// O processo terminou: o que só dá para saber depois (ex.: a sessão que
    /// a ferramenta gravou em arquivo).
    fn finish(&mut self, success: bool) -> Vec<HarnessEvent> {
        let _ = success;
        Vec::new()
    }
}

/// Roda um turno e devolve os eventos em ordem, terminando com
/// [`HarnessMessage::End`]. Soltar o receptor encerra o processo.
pub async fn run(
    spec: CommandSpec,
    mut parser: impl LineParser,
) -> Result<mpsc::Receiver<HarnessMessage>, AgentError> {
    let mut cmd = Command::new(&spec.program);
    cmd.args(&spec.args)
        .envs(spec.env.iter().map(|(k, v)| (k, v)))
        .current_dir(&spec.dir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let mut child = cmd.spawn().map_err(AgentError::Spawn)?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| AgentError::Stdio("stdout não capturado".into()))?;
    let stderr = child.stderr.take();
    let (tx, rx) = mpsc::channel(256);
    let stderr_task = tokio::spawn(async move {
        let mut tail: Vec<String> = Vec::new();
        if let Some(err) = stderr {
            let mut lines = BufReader::new(err).lines();
            while let Ok(Some(l)) = lines.next_line().await {
                tail.push(l);
                if tail.len() > 20 {
                    tail.remove(0);
                }
            }
        }
        tail.join("\n")
    });
    tokio::spawn(async move {
        let mut lines = BufReader::new(stdout).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            for ev in parser.feed(&line) {
                if tx.send(HarnessMessage::Event(ev)).await.is_err() {
                    return; // receptor desistiu: `child` cai e o processo morre
                }
            }
        }
        let success = child.wait().await.map(|s| s.success()).unwrap_or(false);
        for ev in parser.finish(success) {
            let _ = tx.send(HarnessMessage::Event(ev)).await;
        }
        let stderr_tail = stderr_task.await.unwrap_or_default();
        let _ = tx
            .send(HarnessMessage::End(TurnEnd {
                success,
                stderr_tail,
            }))
            .await;
    });
    Ok(rx)
}

/// Binário do Orchestrator ao lado do executável atual, o mesmo lugar onde o
/// setup do Claude Code procura.
pub fn sibling_binary(name: &str) -> Option<PathBuf> {
    let path = std::env::current_exe().ok()?.parent()?.join(name);
    path.exists().then_some(path)
}

/// Uma chamada de ferramenta numa linha, curta.
pub fn tool_line(what: &str, detail: &str) -> String {
    let detail = detail.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut curto: String = detail.chars().take(140).collect();
    if detail.chars().count() > 140 {
        curto.push('…');
    }
    if curto.is_empty() {
        format!("⚙ {what}")
    } else {
        format!("⚙ {what} {curto}")
    }
}

/// Texto como valor TOML (`-c chave=valor` do Codex). As escapes de JSON são
/// um subconjunto das de string básica do TOML.
pub fn toml_str(s: &str) -> String {
    serde_json::to_string(s).unwrap_or_else(|_| "\"\"".to_string())
}

/// Tabela TOML em linha: `{ "CHAVE" = "valor", … }`.
pub fn toml_inline_table(pairs: &[(String, String)]) -> String {
    let itens: Vec<String> = pairs
        .iter()
        .map(|(k, v)| format!("{} = {}", toml_str(k), toml_str(v)))
        .collect();
    format!("{{ {} }}", itens.join(", "))
}

/// `{ "CHAVE": "valor" }` para as configs em JSON (Kimi, OpenCode).
pub fn json_env(pairs: &[(String, String)]) -> serde_json::Value {
    serde_json::Value::Object(
        pairs
            .iter()
            .map(|(k, v)| (k.clone(), serde_json::Value::String(v.clone())))
            .collect(),
    )
}

/// Primeiro texto não vazio entre as chaves dadas.
pub fn text_of<'a>(v: &'a serde_json::Value, keys: &[&str]) -> Option<&'a str> {
    keys.iter()
        .find_map(|k| v.get(*k).and_then(serde_json::Value::as_str))
        .filter(|s| !s.trim().is_empty())
}

/// Mesmo diretório? Compara o caminho como veio e o canônico.
pub fn same_dir(a: &Path, b: &Path) -> bool {
    a == b
        || match (a.canonicalize(), b.canonicalize()) {
            (Ok(x), Ok(y)) => x == y,
            _ => false,
        }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn toml_values_escape_quotes_and_newlines() {
        assert_eq!(toml_str("a \"b\"\nc"), r#""a \"b\"\nc""#);
        let t = toml_inline_table(&[("ORCHESTRATOR_DB".into(), "/x/y.db".into())]);
        assert_eq!(t, r#"{ "ORCHESTRATOR_DB" = "/x/y.db" }"#);
    }

    #[test]
    fn tool_lines_are_short_and_single_line() {
        let longo = "x ".repeat(200);
        let l = tool_line("Shell", &longo);
        assert!(l.chars().count() < 160);
        assert!(l.ends_with('…'));
        assert_eq!(tool_line("cli_status", "  \n "), "⚙ cli_status");
    }

    struct Eco;
    impl LineParser for Eco {
        fn feed(&mut self, line: &str) -> Vec<HarnessEvent> {
            vec![HarnessEvent::Text(line.to_string())]
        }
        fn finish(&mut self, success: bool) -> Vec<HarnessEvent> {
            vec![HarnessEvent::Session(format!("fim-{success}"))]
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn run_streams_lines_then_finish_then_end() {
        let dir = tempfile::tempdir().unwrap();
        let spec = CommandSpec {
            program: "sh".into(),
            args: vec!["-c".into(), "echo um; echo dois; echo falhou >&2; exit 3".into()],
            env: vec![],
            dir: dir.path().to_path_buf(),
        };
        let mut rx = run(spec, Eco).await.unwrap();
        let mut todos = Vec::new();
        while let Some(m) = rx.recv().await {
            todos.push(m);
        }
        assert_eq!(todos[0], HarnessMessage::Event(HarnessEvent::Text("um".into())));
        assert_eq!(todos[1], HarnessMessage::Event(HarnessEvent::Text("dois".into())));
        assert_eq!(todos[2], HarnessMessage::Event(HarnessEvent::Session("fim-false".into())));
        assert_eq!(
            todos[3],
            HarnessMessage::End(TurnEnd { success: false, stderr_tail: "falhou".into() })
        );
    }
}
