//! Antigravity CLI (`agy -p … --output-format stream-json`): o chat do
//! orquestrador na conta Google AI Pro/Ultra.
//!
//! O que se sabe sem conta logada (changelog e nomes de campo do binário
//! 1.2.2): o stream é NDJSON com eventos `init`, `step_update` e o terminal
//! `result`; os campos incluem `conversation_id`, `step_type`, `tool_name`,
//! `tool_info`, `text_delta` e `thinking_tokens`. A forma exata de cada
//! evento ainda precisa ser confirmada numa execução real, então o parser lê
//! esses nomes onde aparecerem, sem supor a estrutura.
//!
//! O agy não tem flag de instruções de sistema nem de MCP por execução: as
//! instruções do maestro vão no começo do prompt (montado por quem chama).

use serde_json::Value;

use crate::harness::{text_of, tool_line, CommandSpec, HarnessEvent, LineParser, TurnRequest};

pub fn command(req: &TurnRequest) -> CommandSpec {
    let mut args: Vec<String> = vec![
        "-p".into(),
        req.prompt.clone(),
        "--output-format".into(),
        "stream-json".into(),
    ];
    if let Some(id) = &req.session {
        args.push("--conversation".into());
        args.push(id.clone());
    }
    if let Some(m) = &req.model {
        args.push("--model".into());
        args.push(m.clone());
    }
    CommandSpec {
        program: req.binary.clone(),
        args,
        env: req.env.clone(),
        dir: req.dir.clone(),
    }
}

#[derive(Debug, Default)]
pub struct Parser {
    session_sent: bool,
    got_delta: bool,
}

/// O valor de uma chave em qualquer nível do objeto (primeiro encontrado).
fn deep<'a>(v: &'a Value, key: &str) -> Option<&'a Value> {
    match v {
        Value::Object(map) => map
            .get(key)
            .or_else(|| map.values().find_map(|x| deep(x, key))),
        Value::Array(items) => items.iter().find_map(|x| deep(x, key)),
        _ => None,
    }
}

impl LineParser for Parser {
    fn feed(&mut self, line: &str) -> Vec<HarnessEvent> {
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        if !self.session_sent {
            if let Some(id) = deep(&v, "conversation_id").and_then(Value::as_str) {
                self.session_sent = true;
                out.push(HarnessEvent::Session(id.to_string()));
            }
        }
        let tipo = text_of(&v, &["type", "event"]).unwrap_or("");
        if let Some(d) = deep(&v, "text_delta").and_then(Value::as_str) {
            if !d.is_empty() {
                self.got_delta = true;
                out.push(HarnessEvent::TextDelta(d.to_string()));
            }
        }
        if let Some(info) = deep(&v, "tool_info") {
            let nome = text_of(info, &["tool_name", "name"])
                .or_else(|| deep(&v, "tool_name").and_then(Value::as_str))
                .unwrap_or("?");
            let params = info
                .get("parameters")
                .or_else(|| info.get("args"))
                .map(Value::to_string)
                .unwrap_or_default();
            out.push(HarnessEvent::Tool(tool_line(nome, &params)));
        }
        if tipo == "result" {
            if !self.got_delta {
                if let Some(t) = text_of(&v, &["result", "text", "response"]) {
                    out.push(HarnessEvent::Text(t.to_string()));
                }
            }
            if let Some(u) = deep(&v, "usage") {
                let n = |k: &[&str]| k.iter().find_map(|k| u.get(*k).and_then(Value::as_u64)).unwrap_or(0);
                out.push(HarnessEvent::Tokens {
                    input: n(&["input_tokens", "prompt_tokens", "inputTokens"]),
                    output: n(&["output_tokens", "completion_tokens", "outputTokens"]),
                    cost: None,
                });
            }
            let erro = v.get("is_error").and_then(Value::as_bool).unwrap_or(false);
            if let Some(e) = text_of(&v, &["error"]).filter(|_| true) {
                out.push(HarnessEvent::Error(e.to_string()));
            } else if erro {
                out.push(HarnessEvent::Error("o Antigravity terminou com erro".into()));
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn command_is_print_mode_with_stream_json_and_no_permission_bypass() {
        let req = TurnRequest {
            binary: "agy".into(),
            dir: PathBuf::from("/p"),
            prompt: "oi".into(),
            session: Some("conv-1".into()),
            ..Default::default()
        };
        let spec = command(&req);
        assert_eq!(&spec.args[..4], ["-p", "oi", "--output-format", "stream-json"]);
        assert!(spec.args.windows(2).any(|w| w == ["--conversation", "conv-1"]));
        assert!(!spec.args.iter().any(|a| a.contains("dangerously")));
    }

    #[test]
    fn reads_the_known_field_names_wherever_they_are() {
        let mut p = Parser::default();
        let eventos: Vec<HarnessEvent> = [
            r#"{"type":"init","conversation_id":"conv-9","model":"auto"}"#,
            r#"{"type":"step_update","step":{"step_type":"planner_response","text_delta":"Olá"}}"#,
            r#"{"type":"step_update","step":{"step_type":"run_command","tool_info":{"tool_name":"run_command","parameters":{"CommandLine":"ls"}}}}"#,
            r#"{"type":"result","result":"Olá","usage":{"input_tokens":10,"output_tokens":3}}"#,
        ]
        .iter()
        .flat_map(|l| p.feed(l))
        .collect();
        assert_eq!(
            eventos,
            vec![
                HarnessEvent::Session("conv-9".into()),
                HarnessEvent::TextDelta("Olá".into()),
                HarnessEvent::Tool("⚙ run_command {\"CommandLine\":\"ls\"}".into()),
                // Texto do resultado não se repete quando já veio em pedaços.
                HarnessEvent::Tokens { input: 10, output: 3, cost: None },
            ]
        );
    }
}
