//! Codex CLI (`codex exec --json`): o chat do orquestrador na conta do ChatGPT.
//!
//! Saída em JSONL. Eventos (documentação do modo não interativo, e nomes
//! conferidos no binário 0.154): `thread.started` com `thread_id`,
//! `item.started`/`item.completed` com `item.type` (`agent_message`,
//! `reasoning`, `command_execution`, `file_change`, `mcp_tool_call`,
//! `web_search`), `turn.completed` com `usage`, `turn.failed` e `error`. Os
//! campos internos de cada item o parser lê com tolerância.
//!
//! O maestro não escreve: `sandbox_mode = "read-only"` e aprovação `never`.
//! As tools do Orchestrator vêm do servidor MCP em modo trava (o Codex só roda
//! hooks cuja definição o usuário revisou, e em `codex exec` pula os outros
//! em silêncio; não dá para depender deles aqui).

use serde_json::Value;

use crate::harness::{
    text_of, toml_inline_table, toml_str, tool_line, CommandSpec, HarnessEvent, LineParser,
    TurnRequest,
};

fn config(args: &mut Vec<String>, key: &str, value: String) {
    args.push("-c".into());
    args.push(format!("{key}={value}"));
}

pub fn command(req: &TurnRequest) -> CommandSpec {
    let mut args: Vec<String> = vec!["exec".into()];
    if let Some(id) = &req.session {
        args.push("resume".into());
        args.push(id.clone());
    }
    args.push("--json".into());
    args.push("--skip-git-repo-check".into());
    config(&mut args, "sandbox_mode", toml_str("read-only"));
    config(&mut args, "approval_policy", toml_str("never"));
    if !req.system.is_empty() {
        config(&mut args, "developer_instructions", toml_str(&req.system));
    }
    if let Some(m) = &req.model {
        args.push("-m".into());
        args.push(m.clone());
    }
    if let Some(mcp) = &req.mcp {
        config(
            &mut args,
            "mcp_servers.orchestrator.command",
            toml_str(&mcp.command.display().to_string()),
        );
        config(&mut args, "mcp_servers.orchestrator.env", toml_inline_table(&mcp.env));
    }
    args.push("--".into());
    args.push(req.prompt.clone());
    CommandSpec {
        program: req.binary.clone(),
        args,
        env: req.env.clone(),
        dir: req.dir.clone(),
    }
}

#[derive(Debug, Default)]
pub struct Parser;

impl LineParser for Parser {
    fn feed(&mut self, line: &str) -> Vec<HarnessEvent> {
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            return Vec::new();
        };
        match v.get("type").and_then(Value::as_str).unwrap_or("") {
            "thread.started" => text_of(&v, &["thread_id", "session_id"])
                .map(|id| vec![HarnessEvent::Session(id.to_string())])
                .unwrap_or_default(),
            "item.completed" => item(v.get("item").unwrap_or(&Value::Null)),
            "turn.completed" => {
                let u = v.get("usage").unwrap_or(&Value::Null);
                let n = |k: &str| u.get(k).and_then(Value::as_u64).unwrap_or(0);
                vec![HarnessEvent::Tokens {
                    input: n("input_tokens"),
                    output: n("output_tokens") + n("reasoning_output_tokens"),
                    cost: None,
                }]
            }
            "turn.failed" => {
                let msg = v
                    .get("error")
                    .and_then(|e| text_of(e, &["message"]))
                    .unwrap_or("o turno falhou");
                vec![HarnessEvent::Error(msg.to_string())]
            }
            "error" => text_of(&v, &["message"])
                .map(|m| vec![HarnessEvent::Error(m.to_string())])
                .unwrap_or_default(),
            _ => Vec::new(),
        }
    }
}

fn item(item: &Value) -> Vec<HarnessEvent> {
    let compacto = |v: Option<&Value>| match v {
        Some(Value::String(s)) => s.clone(),
        Some(outro) if !outro.is_null() => outro.to_string(),
        _ => String::new(),
    };
    match item.get("type").and_then(Value::as_str).unwrap_or("") {
        "agent_message" => text_of(item, &["text", "message"])
            .map(|t| vec![HarnessEvent::Text(t.to_string())])
            .unwrap_or_default(),
        "reasoning" => {
            let texto = text_of(item, &["text"]).map(str::to_string).or_else(|| {
                item.get("summary").and_then(Value::as_array).map(|partes| {
                    partes
                        .iter()
                        .filter_map(|p| p.as_str().or_else(|| p.get("text").and_then(Value::as_str)))
                        .collect::<Vec<_>>()
                        .join("\n")
                })
            });
            texto
                .filter(|t| !t.trim().is_empty())
                .map(|t| vec![HarnessEvent::Thinking(t)])
                .unwrap_or_default()
        }
        "mcp_tool_call" => {
            let server = text_of(item, &["server"]).unwrap_or("mcp");
            let tool = text_of(item, &["tool"]).unwrap_or("?");
            vec![HarnessEvent::Tool(tool_line(
                &format!("{server}.{tool}"),
                &compacto(item.get("arguments")),
            ))]
        }
        "command_execution" => vec![HarnessEvent::Tool(tool_line(
            "shell",
            &compacto(item.get("command")),
        ))],
        "file_change" => {
            let arquivos: Vec<&str> = item
                .get("changes")
                .and_then(Value::as_array)
                .map(|c| c.iter().filter_map(|x| x.get("path").and_then(Value::as_str)).collect())
                .unwrap_or_default();
            vec![HarnessEvent::Tool(tool_line("arquivos", &arquivos.join(", ")))]
        }
        "web_search" => vec![HarnessEvent::Tool(tool_line("busca", &compacto(item.get("query"))))],
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::harness::McpLaunch;
    use std::path::PathBuf;

    fn req() -> TurnRequest {
        TurnRequest {
            binary: "/bin/codex".into(),
            dir: PathBuf::from("/p"),
            prompt: "abra uma CLI".into(),
            system: "Você é o maestro.\nNão escreva código.".into(),
            mcp: Some(McpLaunch {
                command: PathBuf::from("/opt/orchestrator-mcp"),
                env: vec![("ORCHESTRATOR_GATE_IN_MCP".into(), "1".into())],
            }),
            ..Default::default()
        }
    }

    #[test]
    fn new_turn_is_read_only_with_our_mcp_and_instructions() {
        let spec = command(&req());
        assert_eq!(spec.program, "/bin/codex");
        assert_eq!(&spec.args[..3], ["exec", "--json", "--skip-git-repo-check"]);
        let junto = spec.args.join(" ");
        assert!(junto.contains(r#"sandbox_mode="read-only""#), "{junto}");
        assert!(junto.contains(r#"approval_policy="never""#));
        assert!(junto.contains(r#"developer_instructions="Você é o maestro.\nNão escreva código.""#));
        assert!(junto.contains(r#"mcp_servers.orchestrator.command="/opt/orchestrator-mcp""#));
        assert!(junto.contains(r#"mcp_servers.orchestrator.env={ "ORCHESTRATOR_GATE_IN_MCP" = "1" }"#));
        assert_eq!(spec.args[spec.args.len() - 2..], ["--", "abra uma CLI"]);
        assert!(!junto.contains("dangerously"));
    }

    #[test]
    fn resuming_puts_the_session_right_after_resume() {
        let mut r = req();
        r.session = Some("0199a213".into());
        r.model = Some("gpt-5.5".into());
        let spec = command(&r);
        assert_eq!(&spec.args[..3], ["exec", "resume", "0199a213"]);
        assert!(spec.args.windows(2).any(|w| w == ["-m", "gpt-5.5"]));
    }

    #[test]
    fn parses_a_whole_turn() {
        let linhas = [
            r#"{"type":"thread.started","thread_id":"0199a213-81c0-7800-8aa1-bbab2a035a53"}"#,
            r#"{"type":"turn.started"}"#,
            r#"{"type":"item.started","item":{"id":"i2","type":"mcp_tool_call","server":"orchestrator","tool":"cli_start","status":"in_progress"}}"#,
            r#"{"type":"item.completed","item":{"id":"i1","type":"reasoning","text":"Vou abrir a CLI"}}"#,
            r#"{"type":"item.completed","item":{"id":"i2","type":"mcp_tool_call","server":"orchestrator","tool":"cli_start","arguments":{"name":"frontend"},"status":"completed"}}"#,
            r#"{"type":"item.completed","item":{"id":"i3","type":"agent_message","text":"Abri a CLI frontend."}}"#,
            r#"{"type":"turn.completed","usage":{"input_tokens":24763,"cached_input_tokens":24448,"output_tokens":122,"reasoning_output_tokens":8}}"#,
            "linha que não é json",
        ];
        let mut p = Parser;
        let eventos: Vec<HarnessEvent> = linhas.iter().flat_map(|l| p.feed(l)).collect();
        assert_eq!(
            eventos,
            vec![
                HarnessEvent::Session("0199a213-81c0-7800-8aa1-bbab2a035a53".into()),
                HarnessEvent::Thinking("Vou abrir a CLI".into()),
                HarnessEvent::Tool("⚙ orchestrator.cli_start {\"name\":\"frontend\"}".into()),
                HarnessEvent::Text("Abri a CLI frontend.".into()),
                HarnessEvent::Tokens { input: 24763, output: 130, cost: None },
            ]
        );
    }

    #[test]
    fn failures_become_errors() {
        let mut p = Parser;
        assert_eq!(
            p.feed(r#"{"type":"turn.failed","error":{"message":"You've hit your usage limit."}}"#),
            vec![HarnessEvent::Error("You've hit your usage limit.".into())]
        );
        assert_eq!(
            p.feed(r#"{"type":"error","message":"stream disconnected"}"#),
            vec![HarnessEvent::Error("stream disconnected".into())]
        );
    }
}
