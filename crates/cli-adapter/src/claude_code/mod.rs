//! Adaptador para o Claude Code CLI: subprocesso `claude -p` + stream-json.

pub mod capabilities;
pub mod hooks;
pub mod stream;

use std::path::{Path, PathBuf};
use std::process::Stdio;

use async_trait::async_trait;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tokio::sync::mpsc;

use crate::agent::{AgentError, CliAgent, SessionHandle};
use capabilities::CliCapabilities;
use stream::{parse_line, StreamEvent};

/// Opções de execução de um agente, escolhidas por sessão (modelo, esforço,
/// modo de permissão etc.). Só viram flags que a CLI ALVO realmente suporta
/// — checado contra [`CliCapabilities`] descoberto do `--help`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AgentOptions {
    /// Alias ou nome completo do modelo (`haiku`, `sonnet`, `opus`, ...).
    pub model: Option<String>,
    /// Nível de esforço (`low`..`max`), quando a CLI expõe `--effort`.
    pub effort: Option<String>,
    /// Modo de permissão (`plan`, `auto`, `acceptEdits`, ...).
    pub permission_mode: Option<String>,
    /// Modelo(s) de fallback para sobrecarga (`--fallback-model`).
    pub fallback_model: Option<String>,
    /// Teto de gasto em dólares para a sessão (`--max-budget-usd`).
    pub max_budget_usd: Option<f64>,
    /// Tool MCP que responde aos prompts de permissão nativos
    /// (`--permission-prompt-tool mcp__orchestrator__permission_prompt`).
    /// Usado pela postura "perguntar por ferramenta".
    pub permission_prompt_tool: Option<String>,
}

impl AgentOptions {
    /// Nenhuma opção definida?
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }

    /// Converte em argumentos de linha de comando, emitindo apenas flags que
    /// `caps` suporta. Devolve `(args, ignoradas)` — as ignoradas permitem
    /// avisar o usuário do que esta build da CLI não tem.
    pub fn to_args(&self, caps: &CliCapabilities) -> (Vec<String>, Vec<String>) {
        let mut args = Vec::new();
        let mut skipped = Vec::new();
        let mut push = |flag: &str, value: Option<String>| match value {
            Some(v) if caps.supports(flag) => {
                args.push(flag.to_string());
                args.push(v);
            }
            Some(_) => skipped.push(flag.to_string()),
            None => {}
        };
        push("--model", self.model.clone());
        push("--effort", self.effort.clone());
        push("--permission-mode", self.permission_mode.clone());
        push("--fallback-model", self.fallback_model.clone());
        push("--max-budget-usd", self.max_budget_usd.map(|v| v.to_string()));
        push("--permission-prompt-tool", self.permission_prompt_tool.clone());
        (args, skipped)
    }
}

/// Adaptador do Claude Code controlado exclusivamente por subprocesso
/// (`claude -p ... --output-format stream-json --verbose`) — nunca por
/// automação de teclado.
#[derive(Debug, Clone)]
pub struct ClaudeCodeAgent {
    /// Binário a executar (default: `"claude"`).
    pub binary: String,
    /// Argumentos extras acrescentados a toda invocação.
    pub extra_args: Vec<String>,
    /// Opções por sessão (modelo, effort, permission mode, ...).
    pub options: AgentOptions,
    /// Caminho do banco de memória, exportado como `ORCHESTRATOR_DB` para o
    /// hook/MCP acharem a fila de decisões certa. `None` = não define a env.
    pub db_path: Option<PathBuf>,
    /// Nome do projeto, exportado como `ORCHESTRATOR_PROJECT` para o hook/MCP.
    /// `None` = hook cai no basename do cwd.
    pub project_name: Option<String>,
    /// Postura autônoma: exporta `ORCHESTRATOR_AUTONOMOUS=1`, tornando o hook
    /// `PreToolUse` a única autoridade (libera tools limpas explicitamente, sem
    /// travar no prompt nativo). As posturas que pedem confirmação deixam
    /// `false` para o `--permission-prompt-tool`/`--permission-mode` decidirem.
    pub autonomous: bool,
    /// Variáveis extras do subprocesso (ex.: `ANTHROPIC_BASE_URL` e a chave
    /// para o mesmo Claude Code falar com GLM ou MiniMax).
    pub env: Vec<(String, String)>,
    /// Capacidade do canal de eventos.
    pub channel_capacity: usize,
}

impl Default for ClaudeCodeAgent {
    fn default() -> Self {
        Self {
            binary: "claude".to_owned(),
            extra_args: Vec::new(),
            options: AgentOptions::default(),
            db_path: None,
            project_name: None,
            autonomous: false,
            env: Vec::new(),
            channel_capacity: 256,
        }
    }
}

impl ClaudeCodeAgent {
    /// Cria o adaptador apontando para um binário específico.
    pub fn with_binary(binary: impl Into<String>) -> Self {
        Self {
            binary: binary.into(),
            ..Self::default()
        }
    }

    /// Executa um turno: spawna o processo e devolve o receiver de eventos.
    ///
    /// Com `resume_session_id`, acrescenta `--resume <id>` para continuar
    /// uma sessão existente.
    pub async fn run_turn(
        &self,
        project_dir: &Path,
        prompt: &str,
        resume_session_id: Option<&str>,
    ) -> Result<mpsc::Receiver<StreamEvent>, AgentError> {
        let mut cmd = Command::new(&self.binary);
        cmd.arg("-p")
            .arg(prompt)
            .arg("--output-format")
            .arg("stream-json")
            .arg("--verbose");
        if let Some(id) = resume_session_id {
            cmd.arg("--resume").arg(id);
        }
        // Capacidades da build (cacheadas do `--help`, uma vez por binário).
        let caps = capabilities::discover(&self.binary);
        // Streaming token-a-token (deltas de texto/pensamento + usage) só
        // quando a build suporta — senão o stream-json emite só mensagens
        // completas e a UI cai no texto do `result` ao final.
        if caps.supports("--include-partial-messages") {
            cmd.arg("--include-partial-messages");
        }
        // Opções por sessão viram flags só quando a CLI as suporta.
        if !self.options.is_empty() {
            let (args, _skipped) = self.options.to_args(&caps);
            cmd.args(args);
        }
        cmd.envs(self.env.iter().map(|(k, v)| (k, v)));
        // O hook/MCP do orquestrador descobrem banco e projeto por env.
        if let Some(db) = &self.db_path {
            cmd.env("ORCHESTRATOR_DB", db);
        }
        if let Some(project) = &self.project_name {
            cmd.env("ORCHESTRATOR_PROJECT", project);
        }
        // Postura autônoma: o hook libera tools limpas explicitamente.
        if self.autonomous {
            cmd.env("ORCHESTRATOR_AUTONOMOUS", "1");
        }
        cmd.args(&self.extra_args)
            .current_dir(project_dir)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true);

        let mut child = cmd.spawn().map_err(AgentError::Spawn)?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| AgentError::Stdio("stdout não capturado".into()))?;

        let (tx, rx) = mpsc::channel(self.channel_capacity);
        tokio::spawn(async move {
            let mut lines = BufReader::new(stdout).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                if let Some(event) = parse_line(&line) {
                    if tx.send(event).await.is_err() {
                        break; // receptor desistiu
                    }
                }
            }
            let _ = child.wait().await;
        });

        Ok(rx)
    }
}

#[async_trait]
impl CliAgent for ClaudeCodeAgent {
    fn name(&self) -> &str {
        "claude-code"
    }

    fn supports_hooks(&self) -> bool {
        true
    }

    fn supports_mcp(&self) -> bool {
        true
    }

    async fn start_session(
        &self,
        project_dir: &Path,
        prompt: &str,
    ) -> Result<(SessionHandle, mpsc::Receiver<StreamEvent>), AgentError> {
        let mut rx = self.run_turn(project_dir, prompt, None).await?;

        // Aguarda o evento `system/init` para descobrir o session_id,
        // reencaminhando todos os eventos (inclusive o init) ao chamador.
        let (fwd_tx, fwd_rx) = mpsc::channel(self.channel_capacity);
        let mut handle: Option<SessionHandle> = None;
        while let Some(ev) = rx.recv().await {
            if let StreamEvent::SystemInit {
                session_id, model, ..
            } = &ev
            {
                handle = Some(SessionHandle {
                    session_id: session_id.clone(),
                    project_dir: PathBuf::from(project_dir),
                    model: model.clone(),
                });
                let _ = fwd_tx.send(ev).await;
                break;
            }
            let _ = fwd_tx.send(ev).await;
        }
        let handle = handle.ok_or(AgentError::NoResult)?;

        tokio::spawn(async move {
            while let Some(ev) = rx.recv().await {
                if fwd_tx.send(ev).await.is_err() {
                    break;
                }
            }
        });

        Ok((handle, fwd_rx))
    }

    async fn send_turn(
        &self,
        session: &SessionHandle,
        prompt: &str,
    ) -> Result<mpsc::Receiver<StreamEvent>, AgentError> {
        self.run_turn(&session.project_dir, prompt, Some(&session.session_id))
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn caps_with(flags: &[&str]) -> CliCapabilities {
        CliCapabilities {
            flags: flags.iter().map(|f| f.to_string()).collect(),
            ..Default::default()
        }
    }

    #[test]
    fn to_args_emits_only_supported_flags() {
        let opts = AgentOptions {
            model: Some("opus".into()),
            effort: Some("high".into()),
            permission_mode: Some("plan".into()),
            fallback_model: None,
            max_budget_usd: Some(2.5),
            permission_prompt_tool: None,
        };
        let caps = caps_with(&["--model", "--permission-mode"]);
        let (args, skipped) = opts.to_args(&caps);
        assert_eq!(
            args,
            vec!["--model", "opus", "--permission-mode", "plan"]
        );
        assert_eq!(skipped, vec!["--effort", "--max-budget-usd"]);
    }

    #[test]
    fn to_args_emits_permission_prompt_tool_when_supported() {
        let opts = AgentOptions {
            permission_prompt_tool: Some("mcp__orchestrator__permission_prompt".into()),
            ..Default::default()
        };
        // build sem a flag: vira "ignorada".
        let (args, skipped) = opts.to_args(&caps_with(&["--model"]));
        assert!(args.is_empty());
        assert_eq!(skipped, vec!["--permission-prompt-tool"]);
        // build com a flag: emite.
        let (args, skipped) = opts.to_args(&caps_with(&["--permission-prompt-tool"]));
        assert_eq!(
            args,
            vec!["--permission-prompt-tool", "mcp__orchestrator__permission_prompt"]
        );
        assert!(skipped.is_empty());
    }

    #[test]
    fn empty_options_produce_nothing() {
        let opts = AgentOptions::default();
        assert!(opts.is_empty());
        let (args, skipped) = opts.to_args(&caps_with(&["--model"]));
        assert!(args.is_empty());
        assert!(skipped.is_empty());
    }
}
