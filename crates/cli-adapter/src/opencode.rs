//! OpenCode (`opencode run --format json`): o chat do orquestrador com GLM,
//! MiniMax, Kimi, Groq… na chave de cada fornecedor.
//!
//! Conferido no binário 1.18.30: cada linha é
//! `{"type", "timestamp", "sessionID", "part"}` com `type` em `text`,
//! `reasoning`, `tool_use` (parte `{tool, state:{status, input, output,
//! error, title}}`), `step_start`, `step_finish` (parte com `tokens` e
//! `cost`) e `error` (`{"error":{name, data:{message}}}`). Permissão pedida
//! no `run` sem `--auto` é recusada sozinha.
//!
//! Nada é gravado no projeto: servidor MCP e agente do maestro vão em
//! `OPENCODE_CONFIG_CONTENT`, que o OpenCode soma à config do usuário.

use serde_json::{json, Value};

use crate::harness::{json_env, text_of, tool_line, CommandSpec, HarnessEvent, LineParser, TurnRequest};

/// O agente primário do maestro: lê e delega; editar e shell ficam negados.
pub const AGENT: &str = "orchestrator-maestro";

pub fn config_content(req: &TurnRequest) -> String {
    let mut cfg = json!({
        "$schema": "https://opencode.ai/config.json",
        "agent": { AGENT: {
            "description": "Maestro do Orchestrator: lê o projeto e delega às CLIs; não escreve.",
            "mode": "primary",
            "prompt": req.system,
            "tools": { "write": false, "edit": false, "patch": false, "bash": false },
            "permission": { "edit": "deny", "bash": "deny" }
        }}
    });
    if let Some(mcp) = &req.mcp {
        cfg["mcp"] = json!({ "orchestrator": {
            "type": "local",
            "command": [mcp.command.display().to_string()],
            "environment": json_env(&mcp.env),
            "enabled": true
        }});
    }
    cfg.to_string()
}

pub fn command(req: &TurnRequest) -> CommandSpec {
    let mut args: Vec<String> = vec![
        "run".into(),
        "--format".into(),
        "json".into(),
        "--dir".into(),
        req.dir.display().to_string(),
        "--agent".into(),
        AGENT.into(),
    ];
    if let Some(m) = &req.model {
        args.push("--model".into());
        args.push(m.clone());
    }
    if let Some(id) = &req.session {
        args.push("--session".into());
        args.push(id.clone());
    }
    // A mensagem sempre começa pelo contexto do Orchestrator, nunca por "-".
    args.push(req.prompt.clone());
    let mut env = req.env.clone();
    env.push(("OPENCODE_CONFIG_CONTENT".into(), config_content(req)));
    CommandSpec {
        program: req.binary.clone(),
        args,
        env,
        dir: req.dir.clone(),
    }
}

#[derive(Debug, Default)]
pub struct Parser {
    session_sent: bool,
    input: u64,
    output: u64,
    cost: f64,
    has_cost: bool,
}

impl LineParser for Parser {
    fn feed(&mut self, line: &str) -> Vec<HarnessEvent> {
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        if !self.session_sent {
            if let Some(id) = text_of(&v, &["sessionID"]) {
                self.session_sent = true;
                out.push(HarnessEvent::Session(id.to_string()));
            }
        }
        let part = v.get("part").unwrap_or(&Value::Null);
        match v.get("type").and_then(Value::as_str).unwrap_or("") {
            "text" => {
                if let Some(t) = text_of(part, &["text"]) {
                    out.push(HarnessEvent::Text(t.to_string()));
                }
            }
            "reasoning" => {
                if let Some(t) = text_of(part, &["text"]) {
                    out.push(HarnessEvent::Thinking(t.to_string()));
                }
            }
            "tool_use" => {
                let state = part.get("state").unwrap_or(&Value::Null);
                let detalhe = text_of(state, &["title"])
                    .map(str::to_string)
                    .or_else(|| state.get("input").map(Value::to_string))
                    .unwrap_or_default();
                out.push(HarnessEvent::Tool(tool_line(
                    text_of(part, &["tool"]).unwrap_or("?"),
                    &detalhe,
                )));
                if let Some(e) = text_of(state, &["error"]) {
                    out.push(HarnessEvent::Tool(format!("  ✖ {}", e.lines().next().unwrap_or(e))));
                }
            }
            "step_finish" => {
                let t = part.get("tokens").unwrap_or(&Value::Null);
                let n = |k: &str| t.get(k).and_then(Value::as_u64).unwrap_or(0);
                self.input += n("input");
                self.output += n("output") + n("reasoning");
                if let Some(c) = part.get("cost").and_then(Value::as_f64) {
                    self.cost += c;
                    self.has_cost = true;
                }
                out.push(HarnessEvent::Tokens {
                    input: self.input,
                    output: self.output,
                    cost: self.has_cost.then_some(self.cost),
                });
            }
            "error" => {
                let e = v.get("error").unwrap_or(&Value::Null);
                let msg = e
                    .get("data")
                    .and_then(|d| text_of(d, &["message"]))
                    .or_else(|| text_of(e, &["message", "name"]))
                    .map(str::to_string)
                    .unwrap_or_else(|| e.to_string());
                out.push(HarnessEvent::Error(msg));
            }
            _ => {}
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::harness::McpLaunch;
    use std::path::PathBuf;

    #[test]
    fn config_denies_editing_and_brings_our_mcp() {
        let req = TurnRequest {
            system: "maestro".into(),
            mcp: Some(McpLaunch { command: PathBuf::from("/opt/orchestrator-mcp"), env: vec![("ORCHESTRATOR_GATE_IN_MCP".into(), "1".into())] }),
            ..Default::default()
        };
        let v: Value = serde_json::from_str(&config_content(&req)).unwrap();
        let agente = &v["agent"][AGENT];
        assert_eq!(agente["permission"]["edit"], "deny");
        assert_eq!(agente["permission"]["bash"], "deny");
        assert_eq!(agente["prompt"], "maestro");
        assert_eq!(v["mcp"]["orchestrator"]["command"][0], "/opt/orchestrator-mcp");
        assert_eq!(v["mcp"]["orchestrator"]["environment"]["ORCHESTRATOR_GATE_IN_MCP"], "1");
    }

    #[test]
    fn command_uses_json_format_our_agent_and_the_session() {
        let req = TurnRequest {
            binary: "opencode".into(),
            dir: PathBuf::from("/p"),
            prompt: "<orchestrator>oi".into(),
            model: Some("groq/openai/gpt-oss-120b".into()),
            session: Some("ses_1".into()),
            ..Default::default()
        };
        let spec = command(&req);
        assert_eq!(&spec.args[..3], ["run", "--format", "json"]);
        assert!(spec.args.windows(2).any(|w| w == ["--agent", AGENT]));
        assert!(spec.args.windows(2).any(|w| w == ["--model", "groq/openai/gpt-oss-120b"]));
        assert!(spec.args.windows(2).any(|w| w == ["--session", "ses_1"]));
        assert!(spec.env.iter().any(|(k, _)| k == "OPENCODE_CONFIG_CONTENT"));
        assert!(!spec.args.iter().any(|a| a == "--auto"));
    }

    #[test]
    fn parses_session_text_tools_tokens_and_errors() {
        let linhas = [
            r#"{"type":"step_start","timestamp":1,"sessionID":"ses_abc","part":{"type":"step-start"}}"#,
            r#"{"type":"tool_use","timestamp":2,"sessionID":"ses_abc","part":{"type":"tool","tool":"orchestrator_cli_start","state":{"status":"completed","input":{"name":"api"},"output":"ok","title":"cli_start api"}}}"#,
            r#"{"type":"text","timestamp":3,"sessionID":"ses_abc","part":{"type":"text","text":"Abri a CLI api.","time":{"start":1,"end":2}}}"#,
            r#"{"type":"step_finish","timestamp":4,"sessionID":"ses_abc","part":{"type":"step-finish","tokens":{"input":900,"output":40,"reasoning":10,"cache":{"read":0,"write":0}},"cost":0.0012}}"#,
            r#"{"type":"error","timestamp":5,"sessionID":"ses_abc","error":{"name":"APIError","data":{"message":"rate limit"}}}"#,
        ];
        let mut p = Parser::default();
        let eventos: Vec<HarnessEvent> = linhas.iter().flat_map(|l| p.feed(l)).collect();
        assert_eq!(
            eventos,
            vec![
                HarnessEvent::Session("ses_abc".into()),
                HarnessEvent::Tool("⚙ orchestrator_cli_start cli_start api".into()),
                HarnessEvent::Text("Abri a CLI api.".into()),
                HarnessEvent::Tokens { input: 900, output: 50, cost: Some(0.0012) },
                HarnessEvent::Error("rate limit".into()),
            ]
        );
    }
}
