//! `orchestrator-hook` — a trava do Orchestrator dentro de cada ferramenta de IA.
//!
//! Um binário, dois eventos:
//! - antes de cada ferramenta: pergunta a [`gate::decide`] e responde liberar,
//!   bloquear ou "decida você";
//! - a cada prompt (`UserPromptSubmit`): injeta o contexto da memória
//!   ([`gate::prompt_context`]).
//!
//! Cada ferramenta fala um dialeto:
//! - Claude Code, Codex e Kimi Code: payload com `hook_event_name`,
//!   `tool_name`, `tool_input`, `session_id`, `cwd`; resposta em
//!   `hookSpecificOutput` (Kimi documenta o mesmo contrato do Claude);
//! - Antigravity (`agy`): payload em camelCase com `conversationId`,
//!   `workspacePaths` e `toolCall {name, args}`; a resposta é
//!   `{"decision": "allow"|"deny"|"ask", "reason": …}` e é OBRIGATÓRIA —
//!   saída vazia, inválida ou código diferente de zero bloqueia a ferramenta.
//!   Referência: `~/.gemini/antigravity-cli/builtin/skills/agy-customizations/docs/hooks.md`.
//!
//! Quem é quem vem de `ORCHESTRATOR_HARNESS` (exportado por quem abre a
//! ferramenta); sem ela, o formato do payload decide.
//!
//! `--global`: hook instalado na configuração global de uma ferramenta. Fora
//! das sessões do Orchestrator (sem `ORCHESTRATOR_PROJECT`) ele responde o
//! neutro: nada, ou `"ask"` no Antigravity, que devolve a decisão ao fluxo
//! normal de permissões da ferramenta.
//!
//! O projeto vem de `ORCHESTRATOR_PROJECT` ou do basename da pasta de trabalho.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::Result;
use orchestrator_mcp_server::gate::{self, Decision, Options, ToolCall};
use orchestrator_mcp_server::open_store;
use serde_json::{json, Value};

/// A ferramenta de IA que chamou o hook.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Harness {
    Claude,
    Codex,
    Kimi,
    Antigravity,
}

impl Harness {
    fn detect(declared: Option<&str>, payload: &Value) -> Self {
        match declared {
            Some("claude") => Harness::Claude,
            Some("codex") => Harness::Codex,
            Some("kimi") => Harness::Kimi,
            Some("antigravity") => Harness::Antigravity,
            _ if ["toolCall", "conversationId", "workspacePaths"]
                .iter()
                .any(|k| payload.get(*k).is_some()) =>
            {
                Harness::Antigravity
            }
            _ => Harness::Claude,
        }
    }
}

/// O evento, já sem dialeto.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Event {
    Prompt {
        prompt: String,
        cwd: Option<PathBuf>,
    },
    Tool {
        name: String,
        input: String,
        session: String,
        cwd: Option<PathBuf>,
    },
    Other,
}

fn str_of<'a>(v: &'a Value, keys: &[&str]) -> Option<&'a str> {
    keys.iter().find_map(|k| v.get(*k).and_then(Value::as_str))
}

/// Campo de um objeto sem ligar para caixa nem `_` (`ServerName`,
/// `server_name` e `serverName` são o mesmo campo).
fn field<'a>(obj: &'a Value, names: &[&str]) -> Option<&'a Value> {
    let norm = |s: &str| s.to_lowercase().replace('_', "");
    let wanted: Vec<String> = names.iter().map(|n| norm(n)).collect();
    obj.as_object()?
        .iter()
        .find(|(k, _)| wanted.contains(&norm(k)))
        .map(|(_, v)| v)
}

/// Nome e entrada de uma ferramenta do Antigravity. As ferramentas MCP
/// chegam como `call_mcp_tool`, com servidor e ferramenta nos argumentos.
fn antigravity_tool(name: &str, args: &Value) -> (String, String) {
    if name == "call_mcp_tool" || name == "mcp_tool" {
        let server = field(args, &["serverName", "server"]).and_then(Value::as_str).unwrap_or("");
        let tool = field(args, &["toolName", "tool", "name"]).and_then(Value::as_str).unwrap_or("");
        let input = field(args, &["arguments", "args", "input"])
            .map(|v| match v {
                // Às vezes os argumentos vêm como JSON dentro de texto.
                Value::String(s) => s.clone(),
                outro => outro.to_string(),
            })
            .unwrap_or_else(|| args.to_string());
        let nome = if server.contains("orchestrator") {
            gate::canonical_tool_name(tool)
        } else {
            format!("mcp__{server}__{tool}")
        };
        return (nome, input);
    }
    (gate::canonical_tool_name(name), args.to_string())
}

fn parse(harness: Harness, payload: &Value) -> Event {
    if harness == Harness::Antigravity {
        let cwd = payload
            .get("workspacePaths")
            .and_then(|v| v.get(0))
            .and_then(Value::as_str)
            .map(PathBuf::from);
        let Some(call) = payload.get("toolCall") else {
            return Event::Other;
        };
        let (name, input) = antigravity_tool(
            str_of(call, &["name"]).unwrap_or(""),
            call.get("args").unwrap_or(&Value::Null),
        );
        return Event::Tool {
            name,
            input,
            session: str_of(payload, &["conversationId"])
                .unwrap_or("desconhecida")
                .to_string(),
            cwd,
        };
    }
    let cwd = str_of(payload, &["cwd"]).map(PathBuf::from);
    // Confirmado ao vivo no Claude Code: o campo do prompt é `prompt` (a
    // documentação citava `user_input`, que fica como reserva).
    match payload.get("hook_event_name").and_then(Value::as_str) {
        Some("UserPromptSubmit") => Event::Prompt {
            prompt: str_of(payload, &["prompt", "user_input"]).unwrap_or("").to_string(),
            cwd,
        },
        Some("PreToolUse") | None if payload.get("tool_name").is_some() => Event::Tool {
            name: gate::canonical_tool_name(str_of(payload, &["tool_name"]).unwrap_or("")),
            input: payload
                .get("tool_input")
                .map(Value::to_string)
                .unwrap_or_default(),
            session: str_of(payload, &["session_id"])
                .unwrap_or("desconhecida")
                .to_string(),
            cwd,
        },
        _ => Event::Other,
    }
}

/// A resposta: o que vai no stdout e o código de saída.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
struct Reply {
    stdout: Option<String>,
    code: u8,
}

impl Reply {
    fn json(v: Value) -> Self {
        Reply {
            stdout: Some(v.to_string()),
            code: 0,
        }
    }
}

fn render_decision(harness: Harness, decision: &Decision) -> Reply {
    if harness == Harness::Antigravity {
        return Reply::json(match decision {
            Decision::Allow(m) => json!({ "decision": "allow", "reason": m }),
            Decision::Deny(m) => json!({ "decision": "deny", "reason": m }),
            // "Decida você": o fluxo normal de permissões do agy.
            Decision::Silent => json!({ "decision": "ask" }),
        });
    }
    let (decisao, motivo) = match decision {
        Decision::Silent => return Reply::default(),
        Decision::Allow(m) => ("allow", m),
        Decision::Deny(m) => ("deny", m),
    };
    Reply::json(json!({
        "hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "permissionDecision": decisao,
            "permissionDecisionReason": motivo
        }
    }))
}

fn render_context(harness: Harness, text: String) -> Reply {
    match harness {
        Harness::Claude | Harness::Codex => Reply::json(json!({
            "hookSpecificOutput": {
                "hookEventName": "UserPromptSubmit",
                "additionalContext": text
            }
        })),
        // Kimi não documenta a resposta estruturada do prompt: o stdout
        // entra como texto.
        Harness::Kimi => Reply {
            stdout: Some(text),
            code: 0,
        },
        // O Antigravity não passa o prompt aos hooks: a memória vai no
        // próprio prompt, montada por quem abre a ferramenta.
        Harness::Antigravity => Reply::json(json!({})),
    }
}

/// O que responder quando o evento não é conosco (hook global fora de uma
/// sessão do Orchestrator, ou evento que não tratamos).
fn neutral(harness: Harness, event: &Event) -> Reply {
    match (harness, event) {
        (Harness::Antigravity, Event::Tool { .. }) => render_decision(harness, &Decision::Silent),
        (Harness::Antigravity, _) => Reply::json(json!({})),
        _ => Reply::default(),
    }
}

fn main() -> ExitCode {
    let global = std::env::args().skip(1).any(|a| a == "--global");
    let mut input = String::new();
    let _ = std::io::stdin().read_to_string(&mut input);
    let payload: Value = serde_json::from_str(&input).unwrap_or(Value::Null);
    let harness = Harness::detect(
        std::env::var("ORCHESTRATOR_HARNESS").ok().as_deref(),
        &payload,
    );
    let event = parse(harness, &payload);
    let reply = if global && std::env::var_os("ORCHESTRATOR_PROJECT").is_none() {
        neutral(harness, &event)
    } else {
        match run(harness, event) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("orchestrator-hook: {e:#}");
                if harness != Harness::Antigravity {
                    return ExitCode::FAILURE;
                }
                // O agy bloqueia de qualquer jeito; assim o modelo lê o porquê.
                render_decision(
                    harness,
                    &Decision::Deny(format!("a trava do Orchestrator falhou: {e:#}")),
                )
            }
        }
    };
    if let Some(out) = reply.stdout {
        println!("{out}");
    }
    ExitCode::from(reply.code)
}

fn run(harness: Harness, event: Event) -> Result<Reply> {
    let db_path = std::env::var_os("ORCHESTRATOR_DB").map(PathBuf::from);
    match event {
        Event::Prompt { prompt, cwd } => {
            let project = project_of(cwd.as_deref());
            let author = std::env::var("ORCHESTRATOR_AGENT").unwrap_or_else(|_| "IA".to_string());
            Ok(gate::prompt_context(&prompt, &project, &author, db_path)
                .map(|t| render_context(harness, t))
                .unwrap_or_else(|| neutral(harness, &Event::Other)))
        }
        Event::Tool {
            name,
            input,
            session,
            cwd,
        } => {
            let project = project_of(cwd.as_deref());
            // Só SQLite: o hook roda a cada ferramenta e não pode carregar modelo.
            let store = open_store(db_path)?;
            let autonomous = std::env::var("ORCHESTRATOR_AUTONOMOUS")
                .map(|v| v == "1")
                .unwrap_or(false);
            let call = ToolCall {
                tool_name: &name,
                tool_input: &input,
                session_id: &session,
                project: &project,
            };
            let decision = gate::decide(
                &store,
                &call,
                &Options {
                    autonomous,
                    notify: true,
                    // O nome da CLI (ou "orquestrador"): decide quem revisa.
                    requester: std::env::var("ORCHESTRATOR_AGENT").ok(),
                },
            )?;
            Ok(render_decision(harness, &decision))
        }
        Event::Other => Ok(neutral(harness, &Event::Other)),
    }
}

/// Projeto do evento: `ORCHESTRATOR_PROJECT`, senão o nome da pasta.
fn project_of(cwd: Option<&Path>) -> String {
    std::env::var("ORCHESTRATOR_PROJECT").unwrap_or_else(|_| {
        cwd.and_then(|p| p.file_name())
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "default".to_string())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn project_comes_from_the_folder_when_not_given() {
        std::env::remove_var("ORCHESTRATOR_PROJECT");
        assert_eq!(project_of(Some(Path::new("/home/eu/loja"))), "loja");
        assert_eq!(project_of(None), "default");
    }

    #[test]
    fn claude_codex_and_kimi_payloads_read_the_same() {
        // Formato do Kimi conforme a documentação dele (igual ao do Claude).
        let payload = json!({
            "hook_event_name": "PreToolUse",
            "session_id": "abc123",
            "cwd": "/home/eu/loja",
            "tool_name": "mcp__orchestrator__cli_start",
            "tool_input": {"name": "frontend"}
        });
        for declarada in [Some("claude"), Some("codex"), Some("kimi"), None] {
            let h = Harness::detect(declarada, &payload);
            assert_ne!(h, Harness::Antigravity);
            match parse(h, &payload) {
                Event::Tool { name, input, session, cwd } => {
                    assert_eq!(name, "mcp__orchestrator__cli_start");
                    assert!(input.contains("frontend"));
                    assert_eq!(session, "abc123");
                    assert_eq!(cwd.as_deref(), Some(Path::new("/home/eu/loja")));
                }
                outro => panic!("{declarada:?}: {outro:?}"),
            }
        }
    }

    #[test]
    fn antigravity_payload_follows_its_documented_contract() {
        // Exemplo do hooks.md que vem com o agy.
        let payload = json!({
            "toolCall": {"name": "run_command", "args": {"CommandLine": "npm test"}},
            "stepIdx": 19,
            "conversationId": "ec33ebf9-0cba-4100-8142-c61503f6c587",
            "workspacePaths": ["/home/eu/loja"],
            "modelName": "auto"
        });
        let h = Harness::detect(None, &payload);
        assert_eq!(h, Harness::Antigravity);
        match parse(h, &payload) {
            Event::Tool { name, input, session, cwd } => {
                assert_eq!(name, "run_command");
                assert!(input.contains("npm test"));
                assert_eq!(session, "ec33ebf9-0cba-4100-8142-c61503f6c587");
                assert_eq!(cwd.as_deref(), Some(Path::new("/home/eu/loja")));
            }
            outro => panic!("{outro:?}"),
        }
    }

    #[test]
    fn antigravity_mcp_calls_become_our_tool_names() {
        let (nome, entrada) = antigravity_tool(
            "call_mcp_tool",
            &json!({"ServerName": "orchestrator", "ToolName": "cli_send", "Arguments": "{\"name\":\"api\"}"}),
        );
        assert_eq!(nome, "mcp__orchestrator__cli_send");
        assert_eq!(entrada, "{\"name\":\"api\"}");
        let (nome, _) = antigravity_tool("call_mcp_tool", &json!({"serverName": "github", "toolName": "create_issue"}));
        assert_eq!(nome, "mcp__github__create_issue");
    }

    #[test]
    fn prompt_event_carries_the_prompt() {
        let payload = json!({"hook_event_name": "UserPromptSubmit", "prompt": "oi", "cwd": "/x/p"});
        assert_eq!(
            parse(Harness::Claude, &payload),
            Event::Prompt { prompt: "oi".into(), cwd: Some(PathBuf::from("/x/p")) }
        );
    }

    #[test]
    fn each_harness_gets_the_reply_it_understands() {
        let negar = Decision::Deny("consulte a memória".into());
        let codex = render_decision(Harness::Codex, &negar);
        let v: Value = serde_json::from_str(codex.stdout.as_deref().unwrap()).unwrap();
        assert_eq!(v["hookSpecificOutput"]["permissionDecision"], "deny");

        let agy = render_decision(Harness::Antigravity, &negar);
        let v: Value = serde_json::from_str(agy.stdout.as_deref().unwrap()).unwrap();
        assert_eq!(v, json!({"decision": "deny", "reason": "consulte a memória"}));
        assert_eq!(agy.code, 0, "código diferente de zero o agy trata como falha");

        // Sem opinião: nada para quem aceita silêncio; "ask" para o agy, que
        // bloqueia saída vazia.
        for h in [Harness::Claude, Harness::Codex, Harness::Kimi] {
            assert_eq!(render_decision(h, &Decision::Silent), Reply::default());
        }
        let v: Value = serde_json::from_str(
            render_decision(Harness::Antigravity, &Decision::Silent).stdout.as_deref().unwrap(),
        )
        .unwrap();
        assert_eq!(v["decision"], "ask");

        assert_eq!(render_context(Harness::Kimi, "contrato".into()).stdout.as_deref(), Some("contrato"));
        assert!(render_context(Harness::Codex, "contrato".into()).stdout.unwrap().contains("additionalContext"));
    }

    #[test]
    fn outside_an_orchestrator_session_the_global_hook_stays_neutral() {
        let ferramenta = Event::Tool {
            name: "run_command".into(),
            input: "{}".into(),
            session: "s".into(),
            cwd: None,
        };
        assert_eq!(neutral(Harness::Kimi, &ferramenta), Reply::default());
        let agy = neutral(Harness::Antigravity, &ferramenta);
        assert!(agy.stdout.unwrap().contains("\"ask\""));
        assert_eq!(neutral(Harness::Antigravity, &Event::Other).stdout.as_deref(), Some("{}"));
    }
}
