//! Parsing do formato `--output-format stream-json` do Claude Code.
//!
//! Cada linha do stdout é um objeto JSON independente (NDJSON). Variantes
//! desconhecidas nunca causam falha: caem em [`StreamEvent::Unknown`].

use serde::Deserialize;
use serde_json::Value;
use tokio::sync::mpsc;

/// Um evento do stream NDJSON do Claude Code.
///
/// A variante é escolhida pelo campo `"type"`; qualquer tipo ou formato
/// não reconhecido vira [`StreamEvent::Unknown`] com o JSON bruto.
#[derive(Debug, Clone, PartialEq)]
pub enum StreamEvent {
    /// `{"type":"system","subtype":"init",...}` — início de sessão.
    SystemInit {
        session_id: String,
        model: Option<String>,
        raw: Value,
    },
    /// Outros eventos `system` (subtypes futuros).
    System { subtype: Option<String>, raw: Value },
    /// `{"type":"stream_event","event":{...}}` — eventos brutos da API.
    Stream(ApiStreamEvent),
    /// Mensagem completa de topo (`{"type":"assistant"|"user",...}`) —
    /// reconhecida para não poluir; a exibição ao vivo vem dos deltas.
    Message { role: String, raw: Value },
    /// `{"type":"result",...}` — resultado final do turno.
    Result {
        result: Option<String>,
        session_id: Option<String>,
        cost_usd: Option<f64>,
        /// Tokens de entrada totais do turno (do `usage` do `result`).
        input_tokens: Option<u64>,
        /// Tokens de saída totais do turno (do `usage` do `result`).
        output_tokens: Option<u64>,
        is_error: bool,
        raw: Value,
    },
    /// `{"type":"hook_started",...}`.
    HookStarted { raw: Value },
    /// `{"type":"hook_progress",...}`.
    HookProgress { raw: Value },
    /// `{"type":"hook_response",...}`.
    HookResponse { raw: Value },
    /// Qualquer linha JSON não reconhecida (nunca falha o parser).
    Unknown(Value),
}

/// Evento interno de `stream_event.event`, já tipado para os casos úteis.
#[derive(Debug, Clone, PartialEq)]
pub enum ApiStreamEvent {
    /// Início de um bloco de conteúdo (`content_block_start`).
    ContentBlockStart {
        index: Option<u64>,
        /// `Some(name)` quando o bloco é `tool_use`.
        tool_use_name: Option<String>,
        raw: Value,
    },
    /// Delta de texto (`content_block_delta` com `text_delta`).
    TextDelta { text: String },
    /// Delta de pensamento (`content_block_delta` com `thinking_delta`,
    /// campo `thinking`). Só aparece com `--include-partial-messages`.
    ThinkingDelta { text: String },
    /// Contagem de tokens de `message_start` (input) e `message_delta`
    /// (output cumulativo). Campos ausentes ficam `None`.
    Usage {
        input_tokens: Option<u64>,
        output_tokens: Option<u64>,
    },
    /// Pedaço do JSON de entrada de uma tool call (`input_json_delta`).
    /// Chega fatiado; o consumidor acumula por `index` e lê no
    /// [`ApiStreamEvent::ContentBlockStop`] correspondente.
    ToolInputDelta {
        index: Option<u64>,
        partial_json: String,
    },
    /// Fim de um bloco de conteúdo (`content_block_stop`) — momento de
    /// exibir a tool call completa (comando bash, arquivo escrito, ...).
    ContentBlockStop { index: Option<u64> },
    /// Outros deltas (`signature_delta`, ...).
    OtherDelta { raw: Value },
    /// Fim da mensagem (`message_stop`).
    MessageStop,
    /// Qualquer outro evento da API.
    Other(Value),
}

#[derive(Deserialize)]
struct Envelope {
    #[serde(rename = "type")]
    kind: Option<String>,
    subtype: Option<String>,
    session_id: Option<String>,
    model: Option<String>,
    result: Option<Value>,
    #[serde(alias = "cost_usd", alias = "total_cost_usd")]
    cost: Option<f64>,
    is_error: Option<bool>,
    event: Option<Value>,
}

/// Faz o parse de uma linha NDJSON em um [`StreamEvent`].
///
/// Retorna `None` para linhas vazias ou que não são JSON (ex.: logs soltos).
pub fn parse_line(line: &str) -> Option<StreamEvent> {
    let line = line.trim();
    if line.is_empty() {
        return None;
    }
    let raw: Value = serde_json::from_str(line).ok()?;
    Some(parse_value(raw))
}

/// Converte um `serde_json::Value` já parseado em um [`StreamEvent`].
pub fn parse_value(raw: Value) -> StreamEvent {
    let env: Envelope = match serde_json::from_value(raw.clone()) {
        Ok(e) => e,
        Err(_) => return StreamEvent::Unknown(raw),
    };
    match env.kind.as_deref() {
        Some("system") => match (env.subtype.as_deref(), env.session_id) {
            (Some("init"), Some(session_id)) => StreamEvent::SystemInit {
                session_id,
                model: env.model,
                raw,
            },
            _ => StreamEvent::System {
                subtype: env.subtype,
                raw,
            },
        },
        Some("stream_event") => match env.event {
            Some(event) => StreamEvent::Stream(parse_api_event(event)),
            None => StreamEvent::Unknown(raw),
        },
        Some(role @ ("assistant" | "user")) => StreamEvent::Message {
            role: role.to_owned(),
            raw,
        },
        Some("result") => {
            let (input_tokens, output_tokens) = usage_tokens(raw.get("usage"));
            StreamEvent::Result {
                result: env.result.and_then(|v| match v {
                    Value::String(s) => Some(s),
                    other => Some(other.to_string()),
                }),
                session_id: env.session_id,
                cost_usd: env.cost,
                input_tokens,
                output_tokens,
                is_error: env.is_error.unwrap_or(false),
                raw,
            }
        }
        Some("hook_started") => StreamEvent::HookStarted { raw },
        Some("hook_progress") => StreamEvent::HookProgress { raw },
        Some("hook_response") => StreamEvent::HookResponse { raw },
        _ => StreamEvent::Unknown(raw),
    }
}

fn parse_api_event(event: Value) -> ApiStreamEvent {
    let kind = event.get("type").and_then(Value::as_str);
    match kind {
        Some("content_block_start") => {
            let index = event.get("index").and_then(Value::as_u64);
            let block = event.get("content_block");
            let tool_use_name = block
                .filter(|b| b.get("type").and_then(Value::as_str) == Some("tool_use"))
                .and_then(|b| b.get("name"))
                .and_then(Value::as_str)
                .map(str::to_owned);
            ApiStreamEvent::ContentBlockStart {
                index,
                tool_use_name,
                raw: event,
            }
        }
        Some("content_block_delta") => {
            let delta = event.get("delta");
            let delta_kind = delta.and_then(|d| d.get("type")).and_then(Value::as_str);
            match delta_kind {
                Some("text_delta") => {
                    let text = delta
                        .and_then(|d| d.get("text"))
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_owned();
                    ApiStreamEvent::TextDelta { text }
                }
                Some("thinking_delta") => {
                    let text = delta
                        .and_then(|d| d.get("thinking"))
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_owned();
                    ApiStreamEvent::ThinkingDelta { text }
                }
                Some("input_json_delta") => {
                    let partial_json = delta
                        .and_then(|d| d.get("partial_json"))
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_owned();
                    ApiStreamEvent::ToolInputDelta {
                        index: event.get("index").and_then(Value::as_u64),
                        partial_json,
                    }
                }
                _ => ApiStreamEvent::OtherDelta { raw: event },
            }
        }
        Some("content_block_stop") => ApiStreamEvent::ContentBlockStop {
            index: event.get("index").and_then(Value::as_u64),
        },
        // `message_start` carrega o usage inicial (input_tokens) em
        // `message.usage`; `message_delta` traz o output cumulativo em
        // `usage`. Emitimos ambos como Usage para o contador ao vivo.
        Some("message_start") => {
            let (input_tokens, output_tokens) =
                usage_tokens(event.get("message").and_then(|m| m.get("usage")));
            if input_tokens.is_some() || output_tokens.is_some() {
                ApiStreamEvent::Usage {
                    input_tokens,
                    output_tokens,
                }
            } else {
                ApiStreamEvent::Other(event)
            }
        }
        Some("message_delta") => {
            let (input_tokens, output_tokens) = usage_tokens(event.get("usage"));
            if input_tokens.is_some() || output_tokens.is_some() {
                ApiStreamEvent::Usage {
                    input_tokens,
                    output_tokens,
                }
            } else {
                ApiStreamEvent::Other(event)
            }
        }
        Some("message_stop") => ApiStreamEvent::MessageStop,
        _ => ApiStreamEvent::Other(event),
    }
}

/// Extrai `(input_tokens, output_tokens)` de um objeto `usage`, tolerando
/// ausência do objeto ou de campos individuais.
fn usage_tokens(usage: Option<&Value>) -> (Option<u64>, Option<u64>) {
    match usage {
        Some(u) => (
            u.get("input_tokens").and_then(Value::as_u64),
            u.get("output_tokens").and_then(Value::as_u64),
        ),
        None => (None, None),
    }
}

/// Resultado consolidado de um turno.
#[derive(Debug, Clone, PartialEq)]
pub struct TurnResult {
    /// Sessão à qual o turno pertence (do `init` ou do `result`).
    pub session_id: Option<String>,
    /// Texto final: o campo `result`, ou a concatenação dos text deltas.
    pub final_text: String,
    /// Custo em USD, quando reportado.
    pub cost_usd: Option<f64>,
    /// `true` se o agente reportou erro no resultado.
    pub is_error: bool,
}

/// Drena um receiver de eventos e consolida `(session_id, texto final)`.
///
/// Prefere o campo `result` do evento final; na ausência dele, usa a
/// concatenação dos `text_delta` observados.
pub async fn collect_result(mut rx: mpsc::Receiver<StreamEvent>) -> TurnResult {
    let mut session_id: Option<String> = None;
    let mut deltas = String::new();
    let mut final_text: Option<String> = None;
    let mut cost_usd = None;
    let mut is_error = false;

    while let Some(ev) = rx.recv().await {
        match ev {
            StreamEvent::SystemInit { session_id: id, .. } => {
                session_id.get_or_insert(id);
            }
            StreamEvent::Stream(ApiStreamEvent::TextDelta { text }) => deltas.push_str(&text),
            StreamEvent::Result {
                result,
                session_id: id,
                cost_usd: cost,
                is_error: err,
                ..
            } => {
                if let Some(id) = id {
                    session_id = Some(id);
                }
                final_text = result;
                cost_usd = cost;
                is_error = err;
            }
            _ => {}
        }
    }

    TurnResult {
        session_id,
        final_text: final_text.unwrap_or(deltas),
        cost_usd,
        is_error,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_system_init() {
        let ev = parse_line(
            r#"{"type":"system","subtype":"init","session_id":"abc-123","model":"claude-x","tools":[]}"#,
        )
        .unwrap();
        match ev {
            StreamEvent::SystemInit {
                session_id, model, ..
            } => {
                assert_eq!(session_id, "abc-123");
                assert_eq!(model.as_deref(), Some("claude-x"));
            }
            other => panic!("esperado SystemInit, veio {other:?}"),
        }
    }

    #[test]
    fn parse_text_delta_and_message_stop() {
        let ev = parse_line(
            r#"{"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"olá"}}}"#,
        )
        .unwrap();
        assert_eq!(
            ev,
            StreamEvent::Stream(ApiStreamEvent::TextDelta {
                text: "olá".into()
            })
        );
        let ev = parse_line(r#"{"type":"stream_event","event":{"type":"message_stop"}}"#).unwrap();
        assert_eq!(ev, StreamEvent::Stream(ApiStreamEvent::MessageStop));
    }

    #[test]
    fn parse_thinking_delta() {
        let ev = parse_line(
            r#"{"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"deixa eu pensar"}}}"#,
        )
        .unwrap();
        assert_eq!(
            ev,
            StreamEvent::Stream(ApiStreamEvent::ThinkingDelta {
                text: "deixa eu pensar".into()
            })
        );
    }

    #[test]
    fn parse_usage_from_message_start_and_delta() {
        let ev = parse_line(
            r#"{"type":"stream_event","event":{"type":"message_start","message":{"usage":{"input_tokens":42,"output_tokens":1}}}}"#,
        )
        .unwrap();
        assert_eq!(
            ev,
            StreamEvent::Stream(ApiStreamEvent::Usage {
                input_tokens: Some(42),
                output_tokens: Some(1),
            })
        );
        let ev = parse_line(
            r#"{"type":"stream_event","event":{"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":99}}}"#,
        )
        .unwrap();
        assert_eq!(
            ev,
            StreamEvent::Stream(ApiStreamEvent::Usage {
                input_tokens: None,
                output_tokens: Some(99),
            })
        );
    }

    #[test]
    fn parse_result_captures_usage() {
        let ev = parse_line(
            r#"{"type":"result","result":"ok","session_id":"s1","total_cost_usd":0.02,"is_error":false,"usage":{"input_tokens":100,"output_tokens":50}}"#,
        )
        .unwrap();
        match ev {
            StreamEvent::Result {
                input_tokens,
                output_tokens,
                cost_usd,
                ..
            } => {
                assert_eq!(input_tokens, Some(100));
                assert_eq!(output_tokens, Some(50));
                assert_eq!(cost_usd, Some(0.02));
            }
            other => panic!("esperado Result, veio {other:?}"),
        }
    }

    #[test]
    fn parse_top_level_assistant_is_message() {
        let ev = parse_line(
            r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"oi"}]},"session_id":"s1"}"#,
        )
        .unwrap();
        match ev {
            StreamEvent::Message { role, .. } => assert_eq!(role, "assistant"),
            other => panic!("esperado Message, veio {other:?}"),
        }
    }

    #[test]
    fn parse_tool_use_block_start() {
        let ev = parse_line(
            r#"{"type":"stream_event","event":{"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"tu1","name":"Bash","input":{}}}}"#,
        )
        .unwrap();
        match ev {
            StreamEvent::Stream(ApiStreamEvent::ContentBlockStart {
                index,
                tool_use_name,
                ..
            }) => {
                assert_eq!(index, Some(1));
                assert_eq!(tool_use_name.as_deref(), Some("Bash"));
            }
            other => panic!("esperado ContentBlockStart, veio {other:?}"),
        }
    }

    #[test]
    fn parse_result_and_hooks() {
        let ev = parse_line(
            r#"{"type":"result","result":"pronto","session_id":"abc-123","total_cost_usd":0.05,"is_error":false}"#,
        )
        .unwrap();
        match ev {
            StreamEvent::Result {
                result,
                session_id,
                cost_usd,
                is_error,
                ..
            } => {
                assert_eq!(result.as_deref(), Some("pronto"));
                assert_eq!(session_id.as_deref(), Some("abc-123"));
                assert_eq!(cost_usd, Some(0.05));
                assert!(!is_error);
            }
            other => panic!("esperado Result, veio {other:?}"),
        }
        assert!(matches!(
            parse_line(r#"{"type":"hook_started","hook":"PreToolUse"}"#).unwrap(),
            StreamEvent::HookStarted { .. }
        ));
        assert!(matches!(
            parse_line(r#"{"type":"hook_progress"}"#).unwrap(),
            StreamEvent::HookProgress { .. }
        ));
        assert!(matches!(
            parse_line(r#"{"type":"hook_response","ok":true}"#).unwrap(),
            StreamEvent::HookResponse { .. }
        ));
    }

    #[test]
    fn unknown_variants_do_not_fail() {
        assert!(matches!(
            parse_line(r#"{"type":"totally_new_thing","x":1}"#).unwrap(),
            StreamEvent::Unknown(_)
        ));
        assert!(matches!(
            parse_line(r#"{"no_type_at_all":true}"#).unwrap(),
            StreamEvent::Unknown(_)
        ));
        // Linhas não-JSON e vazias são ignoradas.
        assert!(parse_line("not json").is_none());
        assert!(parse_line("   ").is_none());
    }

    #[tokio::test]
    async fn collect_result_prefers_result_field() {
        let (tx, rx) = mpsc::channel(16);
        let lines = [
            r#"{"type":"system","subtype":"init","session_id":"s1","model":"m"}"#,
            r#"{"type":"stream_event","event":{"type":"content_block_delta","delta":{"type":"text_delta","text":"par"}}}"#,
            r#"{"type":"stream_event","event":{"type":"content_block_delta","delta":{"type":"text_delta","text":"cial"}}}"#,
            r#"{"type":"result","result":"final","session_id":"s1","cost_usd":0.01}"#,
        ];
        for l in lines {
            tx.send(parse_line(l).unwrap()).await.unwrap();
        }
        drop(tx);
        let res = collect_result(rx).await;
        assert_eq!(res.session_id.as_deref(), Some("s1"));
        assert_eq!(res.final_text, "final");
        assert_eq!(res.cost_usd, Some(0.01));
    }

    #[tokio::test]
    async fn collect_result_falls_back_to_deltas() {
        let (tx, rx) = mpsc::channel(16);
        for l in [
            r#"{"type":"system","subtype":"init","session_id":"s2"}"#,
            r#"{"type":"stream_event","event":{"type":"content_block_delta","delta":{"type":"text_delta","text":"olá "}}}"#,
            r#"{"type":"stream_event","event":{"type":"content_block_delta","delta":{"type":"text_delta","text":"mundo"}}}"#,
        ] {
            tx.send(parse_line(l).unwrap()).await.unwrap();
        }
        drop(tx);
        let res = collect_result(rx).await;
        assert_eq!(res.session_id.as_deref(), Some("s2"));
        assert_eq!(res.final_text, "olá mundo");
    }
}

/// Acumula os `input_json_delta` de uma tool call e devolve uma linha legível
/// quando o bloco fecha.
///
/// Existe porque o card headless só mostrava `⚙ [Bash]` — o usuário pediu
/// para VER o comando executado, o arquivo escrito e as linhas mudadas. O
/// JSON chega fatiado, então guardamos por índice de bloco.
#[derive(Debug, Default)]
pub struct ToolCallTracker {
    /// (índice do bloco) → (nome da tool, JSON acumulado).
    blocks: std::collections::HashMap<u64, (String, String)>,
}

impl ToolCallTracker {
    pub fn new() -> Self {
        Self::default()
    }

    /// Registra o início de um bloco `tool_use`.
    pub fn start(&mut self, index: Option<u64>, tool: &str) {
        self.blocks
            .insert(index.unwrap_or(0), (tool.to_string(), String::new()));
    }

    /// Acumula um pedaço do JSON de entrada da tool.
    pub fn push(&mut self, index: Option<u64>, partial: &str) {
        if let Some((_, json)) = self.blocks.get_mut(&index.unwrap_or(0)) {
            json.push_str(partial);
        }
    }

    /// Fecha o bloco e devolve a linha a exibir (`None` se não era tool call
    /// ou se o JSON veio incompleto/vazio).
    pub fn finish(&mut self, index: Option<u64>) -> Option<String> {
        let (tool, json) = self.blocks.remove(&index.unwrap_or(0))?;
        Some(describe_tool_call(&tool, &json))
    }
}

/// Uma linha legível para uma tool call: prioriza o que o usuário quer ver
/// (comando, caminho, quantas linhas mudaram).
pub fn describe_tool_call(tool: &str, input_json: &str) -> String {
    let v: Value = serde_json::from_str(input_json).unwrap_or(Value::Null);
    let get = |k: &str| v.get(k).and_then(Value::as_str).unwrap_or("").to_string();
    let detail = match tool {
        "Bash" => {
            let cmd = get("command");
            if cmd.is_empty() {
                String::new()
            } else {
                cmd
            }
        }
        "Write" => {
            let path = get("file_path");
            let content = get("content");
            let n = content.lines().count();
            if path.is_empty() {
                String::new()
            } else if n > 0 {
                format!("{path} ({n} linhas)")
            } else {
                path
            }
        }
        "Edit" | "MultiEdit" | "NotebookEdit" => {
            let path = get("file_path").replace("notebook_path", "");
            let old = get("old_string");
            let new = get("new_string");
            let (minus, plus) = (old.lines().count(), new.lines().count());
            if path.is_empty() {
                String::new()
            } else if minus > 0 || plus > 0 {
                format!("{path} (-{minus}/+{plus})")
            } else {
                path
            }
        }
        "Read" | "Glob" | "Grep" => {
            let path = get("file_path");
            let pattern = get("pattern");
            match (path.is_empty(), pattern.is_empty()) {
                (false, true) => path,
                (true, false) => pattern,
                (false, false) => format!("{pattern} em {path}"),
                _ => String::new(),
            }
        }
        _ => {
            // Tool desconhecida: mostra o primeiro campo string do input.
            v.as_object()
                .and_then(|o| {
                    o.iter()
                        .find_map(|(k, val)| val.as_str().map(|s| format!("{k}={s}")))
                })
                .unwrap_or_default()
        }
    };
    let detail = detail.replace('\n', " ⏎ ");
    let detail = if detail.chars().count() > 160 {
        let cut: String = detail.chars().take(157).collect();
        format!("{cut}...")
    } else {
        detail
    };
    if detail.is_empty() {
        format!("⚙ {tool}")
    } else {
        format!("⚙ {tool}: {detail}")
    }
}

#[cfg(test)]
mod tool_detail_tests {
    use super::*;

    #[test]
    fn parses_input_json_delta_and_block_stop() {
        let ev = parse_line(
            r#"{"type":"stream_event","event":{"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"{\"comm"}}}"#,
        )
        .expect("linha deveria parsear");
        match ev {
            StreamEvent::Stream(ApiStreamEvent::ToolInputDelta {
                index,
                partial_json,
            }) => {
                assert_eq!(index, Some(1));
                assert_eq!(partial_json, "{\"comm");
            }
            other => panic!("esperado ToolInputDelta, veio {other:?}"),
        }
        let ev = parse_line(
            r#"{"type":"stream_event","event":{"type":"content_block_stop","index":1}}"#,
        )
        .expect("linha deveria parsear");
        assert_eq!(
            ev,
            StreamEvent::Stream(ApiStreamEvent::ContentBlockStop { index: Some(1) })
        );
    }

    #[test]
    fn tracker_assembles_bash_command_across_deltas() {
        let mut t = ToolCallTracker::new();
        t.start(Some(0), "Bash");
        t.push(Some(0), "{\"command\": \"cargo ");
        t.push(Some(0), "test --all\"}");
        assert_eq!(
            t.finish(Some(0)).unwrap(),
            "⚙ Bash: cargo test --all"
        );
        // Bloco já consumido não repete.
        assert!(t.finish(Some(0)).is_none());
    }

    #[test]
    fn describes_write_with_line_count_and_edit_with_diff() {
        assert_eq!(
            describe_tool_call("Write", r#"{"file_path":"/tmp/a.rs","content":"um\ndois\ntres"}"#),
            "⚙ Write: /tmp/a.rs (3 linhas)"
        );
        assert_eq!(
            describe_tool_call(
                "Edit",
                r#"{"file_path":"/tmp/a.rs","old_string":"um\ndois","new_string":"um"}"#
            ),
            "⚙ Edit: /tmp/a.rs (-2/+1)"
        );
    }

    #[test]
    fn describes_read_and_grep_and_unknown_tools() {
        assert_eq!(
            describe_tool_call("Read", r#"{"file_path":"/tmp/a.rs"}"#),
            "⚙ Read: /tmp/a.rs"
        );
        assert_eq!(
            describe_tool_call("Grep", r#"{"pattern":"fn main","file_path":"src"}"#),
            "⚙ Grep: fn main em src"
        );
        assert_eq!(
            describe_tool_call("mcp__orchestrator__cli_send", r#"{"name":"frontend"}"#),
            "⚙ mcp__orchestrator__cli_send: name=frontend"
        );
    }

    #[test]
    fn falls_back_to_tool_name_on_broken_json() {
        assert_eq!(describe_tool_call("Bash", "{incompleto"), "⚙ Bash");
        assert_eq!(describe_tool_call("Bash", ""), "⚙ Bash");
    }

    #[test]
    fn long_detail_is_truncated_and_newlines_flattened() {
        let long = "x".repeat(300);
        let out = describe_tool_call("Bash", &format!(r#"{{"command":"{long}"}}"#));
        assert!(out.ends_with("..."));
        assert!(out.chars().count() < 200);
        let multi = describe_tool_call("Bash", r#"{"command":"um\ndois"}"#);
        assert_eq!(multi, "⚙ Bash: um ⏎ dois");
    }
}

