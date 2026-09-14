//! Kimi Code CLI (`kimi --print --output-format stream-json`): o chat do
//! orquestrador na conta Kimi.
//!
//! Conferido no código do pacote instalado (kimi-cli 1.50,
//! `kimi_cli/ui/print/visualize.py` e `metadata.py`):
//! - cada linha é uma mensagem: `{"role":"assistant","content":…,
//!   "tool_calls":[{"id","function":{"name","arguments"}}]}` ou
//!   `{"role":"tool","tool_call_id",…}`; `content` é texto ou lista de partes
//!   (`{"type":"text","text"}`, `{"type":"think","think"}`);
//! - o modo print não imprime a sessão nem os tokens. A sessão fica em
//!   `~/.kimi/kimi.json` (`work_dirs[].last_session_id` da pasta) e é lida
//!   quando o processo termina;
//! - no modo print toda ferramenta é aprovada sozinha. O maestro roda com um
//!   agente que herda o padrão e tira as de escrita, shell e subagente; as
//!   instruções entram em `ROLE_ADDITIONAL`, que o prompt padrão reserva para
//!   isso.

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use crate::harness::{
    json_env, same_dir, text_of, tool_line, CommandSpec, HarnessEvent, LineParser, TurnRequest,
};

/// Ferramentas que o maestro não tem.
pub const EXCLUDED_TOOLS: &[&str] = &[
    "kimi_cli.tools.shell:Shell",
    "kimi_cli.tools.file:WriteFile",
    "kimi_cli.tools.file:StrReplaceFile",
    "kimi_cli.tools.agent:Agent",
    "kimi_cli.tools.background:TaskStop",
];

/// O agente do maestro. JSON é YAML válido, e assim o texto das instruções
/// não depende de indentação.
pub fn agent_spec(system: &str) -> String {
    json!({
        "version": 1,
        "agent": {
            "extend": "default",
            "name": "orchestrator-maestro",
            "system_prompt_args": { "ROLE_ADDITIONAL": system },
            "exclude_tools": EXCLUDED_TOOLS,
        }
    })
    .to_string()
}

pub fn command(req: &TurnRequest) -> std::io::Result<CommandSpec> {
    let pasta = req.state_dir.join("kimi");
    fs::create_dir_all(&pasta)?;
    let arquivo = pasta.join("maestro.yaml");
    let spec = agent_spec(&req.system);
    if fs::read_to_string(&arquivo).ok().as_deref() != Some(spec.as_str()) {
        fs::write(&arquivo, &spec)?;
    }
    let mut args: Vec<String> = vec![
        "--print".into(),
        "--output-format".into(),
        "stream-json".into(),
        "--work-dir".into(),
        req.dir.display().to_string(),
        "--agent-file".into(),
        arquivo.display().to_string(),
    ];
    if let Some(mcp) = &req.mcp {
        args.push("--mcp-config".into());
        args.push(
            json!({ "mcpServers": { "orchestrator": {
                "command": mcp.command.display().to_string(),
                "args": [],
                "env": json_env(&mcp.env),
            }}})
            .to_string(),
        );
    }
    if let Some(m) = &req.model {
        args.push("--model".into());
        args.push(m.clone());
    }
    if let Some(id) = &req.session {
        args.push("--session".into());
        args.push(id.clone());
    }
    // `=`: um prompt que comece com "-" não vira opção.
    args.push(format!("--prompt={}", req.prompt));
    Ok(CommandSpec {
        program: req.binary.clone(),
        args,
        env: req.env.clone(),
        dir: req.dir.clone(),
    })
}

/// `$KIMI_SHARE_DIR`, senão `~/.kimi`.
pub fn share_dir() -> PathBuf {
    std::env::var_os("KIMI_SHARE_DIR")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            std::env::var_os("HOME")
                .map(PathBuf::from)
                .unwrap_or_default()
                .join(".kimi")
        })
}

/// A última sessão do Kimi nesta pasta.
pub fn last_session(share_dir: &Path, work_dir: &Path) -> Option<String> {
    let texto = fs::read_to_string(share_dir.join("kimi.json")).ok()?;
    let v: Value = serde_json::from_str(&texto).ok()?;
    v.get("work_dirs")?
        .as_array()?
        .iter()
        .filter(|wd| {
            wd.get("path")
                .and_then(Value::as_str)
                .is_some_and(|p| same_dir(Path::new(p), work_dir))
        })
        .find_map(|wd| wd.get("last_session_id").and_then(Value::as_str))
        .map(str::to_string)
}

pub struct Parser {
    work_dir: PathBuf,
    share_dir: PathBuf,
}

impl Parser {
    pub fn new(work_dir: &Path) -> Self {
        Self::with_share_dir(work_dir, share_dir())
    }

    pub fn with_share_dir(work_dir: &Path, share_dir: PathBuf) -> Self {
        Self {
            work_dir: work_dir.to_path_buf(),
            share_dir,
        }
    }
}

impl LineParser for Parser {
    fn feed(&mut self, line: &str) -> Vec<HarnessEvent> {
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            return Vec::new();
        };
        if v.get("role").and_then(Value::as_str) != Some("assistant") {
            return Vec::new();
        }
        let mut out = Vec::new();
        match v.get("content") {
            Some(Value::String(s)) if !s.trim().is_empty() => out.push(HarnessEvent::Text(s.clone())),
            Some(Value::Array(partes)) => {
                let mut texto = String::new();
                for parte in partes {
                    match parte.get("type").and_then(Value::as_str) {
                        Some("text") => texto.push_str(parte.get("text").and_then(Value::as_str).unwrap_or("")),
                        Some("think") => {
                            if let Some(t) = text_of(parte, &["think", "text"]) {
                                out.push(HarnessEvent::Thinking(t.to_string()));
                            }
                        }
                        _ => {}
                    }
                }
                if !texto.trim().is_empty() {
                    out.push(HarnessEvent::Text(texto));
                }
            }
            _ => {}
        }
        for call in v.get("tool_calls").and_then(Value::as_array).into_iter().flatten() {
            let f = call.get("function").unwrap_or(&Value::Null);
            out.push(HarnessEvent::Tool(tool_line(
                text_of(f, &["name"]).unwrap_or("?"),
                text_of(f, &["arguments"]).unwrap_or(""),
            )));
        }
        out
    }

    fn finish(&mut self, _success: bool) -> Vec<HarnessEvent> {
        last_session(&self.share_dir, &self.work_dir)
            .map(|id| vec![HarnessEvent::Session(id)])
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::harness::McpLaunch;

    #[test]
    fn maestro_agent_extends_default_without_writing_tools() {
        let v: Value = serde_json::from_str(&agent_spec("Não escreva código.\nDelegue.")).unwrap();
        assert_eq!(v["agent"]["extend"], "default");
        assert_eq!(v["agent"]["system_prompt_args"]["ROLE_ADDITIONAL"], "Não escreva código.\nDelegue.");
        let fora: Vec<&str> = v["agent"]["exclude_tools"].as_array().unwrap().iter().map(|t| t.as_str().unwrap()).collect();
        for t in ["kimi_cli.tools.shell:Shell", "kimi_cli.tools.file:WriteFile", "kimi_cli.tools.file:StrReplaceFile"] {
            assert!(fora.contains(&t), "{t}");
        }
    }

    #[test]
    fn command_writes_the_agent_and_passes_our_mcp() {
        let estado = tempfile::tempdir().unwrap();
        let req = TurnRequest {
            binary: "kimi".into(),
            dir: PathBuf::from("/projeto"),
            prompt: "-oi".into(),
            system: "maestro".into(),
            session: Some("s-1".into()),
            mcp: Some(McpLaunch { command: PathBuf::from("/opt/orchestrator-mcp"), env: vec![("ORCHESTRATOR_PROJECT".into(), "loja".into())] }),
            state_dir: estado.path().to_path_buf(),
            ..Default::default()
        };
        let spec = command(&req).unwrap();
        let a = &spec.args;
        assert!(a.windows(2).any(|w| w == ["--output-format", "stream-json"]));
        assert!(a.windows(2).any(|w| w == ["--work-dir", "/projeto"]));
        assert!(a.windows(2).any(|w| w == ["--session", "s-1"]));
        assert_eq!(a.last().unwrap(), "--prompt=-oi");
        let i = a.iter().position(|x| x == "--mcp-config").unwrap();
        let mcp: Value = serde_json::from_str(&a[i + 1]).unwrap();
        assert_eq!(mcp["mcpServers"]["orchestrator"]["env"]["ORCHESTRATOR_PROJECT"], "loja");
        let agente = estado.path().join("kimi/maestro.yaml");
        assert!(fs::read_to_string(agente).unwrap().contains("orchestrator-maestro"));
        assert!(!a.iter().any(|x| x == "--yolo"));
    }

    #[test]
    fn parses_assistant_text_thinking_and_tool_calls() {
        let mut p = Parser::with_share_dir(Path::new("/p"), PathBuf::from("/nao/existe"));
        assert_eq!(
            p.feed(r#"{"role":"assistant","content":"Let me check the current directory.","tool_calls":[{"type":"function","id":"tc_1","function":{"name":"Shell","arguments":"{\"command\":\"ls\"}"}}]}"#),
            vec![
                HarnessEvent::Text("Let me check the current directory.".into()),
                HarnessEvent::Tool("⚙ Shell {\"command\":\"ls\"}".into()),
            ]
        );
        assert_eq!(
            p.feed(r#"{"role":"assistant","content":[{"type":"think","think":"pensando"},{"type":"text","text":"Pronto."}]}"#),
            vec![HarnessEvent::Thinking("pensando".into()), HarnessEvent::Text("Pronto.".into())]
        );
        assert!(p.feed(r#"{"role":"tool","tool_call_id":"tc_1","content":"file1.py"}"#).is_empty());
        assert!(p.finish(true).is_empty());
    }

    #[test]
    fn session_comes_from_kimi_json_for_this_folder() {
        let share = tempfile::tempdir().unwrap();
        fs::write(
            share.path().join("kimi.json"),
            r#"{"work_dirs":[{"path":"/outro","kaos":"local","last_session_id":"x"},{"path":"/projeto","kaos":"local","last_session_id":"abc-123"}]}"#,
        )
        .unwrap();
        let mut p = Parser::with_share_dir(Path::new("/projeto"), share.path().to_path_buf());
        assert_eq!(p.finish(true), vec![HarnessEvent::Session("abc-123".into())]);
    }
}
