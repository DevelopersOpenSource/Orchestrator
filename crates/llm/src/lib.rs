//! Cliente HTTP mínimo para APIs de chat **compatíveis com OpenAI**
//! (`POST {base_url}/chat/completions`).
//!
//! Cobre com um único código: Ollama (`http://localhost:11434/v1`),
//! LM Studio (`http://localhost:1234/v1`), Groq, OpenRouter, Perplexity,
//! NVIDIA NIM e qualquer outro provedor que fale o mesmo dialeto.

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

/// Uma mensagem do histórico de chat.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatMessage {
    /// `"system"`, `"user"` ou `"assistant"`.
    pub role: String,
    pub content: String,
}

impl ChatMessage {
    pub fn system(content: impl Into<String>) -> Self {
        Self { role: "system".into(), content: content.into() }
    }
    pub fn user(content: impl Into<String>) -> Self {
        Self { role: "user".into(), content: content.into() }
    }
    pub fn assistant(content: impl Into<String>) -> Self {
        Self { role: "assistant".into(), content: content.into() }
    }
}

/// Chama `{base_url}/chat/completions` e retorna o texto da resposta.
///
/// `api_key = None` para servidores locais sem autenticação
/// (Ollama, LM Studio).
pub async fn chat_completion(
    base_url: &str,
    api_key: Option<&str>,
    model: &str,
    messages: &[ChatMessage],
) -> Result<String> {
    let url = format!("{}/chat/completions", base_url.trim_end_matches('/'));
    let client = reqwest::Client::new();
    let mut req = client.post(&url).json(&serde_json::json!({
        "model": model,
        "messages": messages,
        "stream": false,
    }));
    if let Some(key) = api_key {
        req = req.bearer_auth(key);
    }
    let resp = req
        .send()
        .await
        .with_context(|| format!("chamando {url}"))?;
    let status = resp.status();
    let body = resp.text().await.context("lendo corpo da resposta")?;
    if !status.is_success() {
        bail!("HTTP {status} de {url}: {}", truncate(&body, 300));
    }
    let value: serde_json::Value =
        serde_json::from_str(&body).with_context(|| format!("resposta não é JSON: {}", truncate(&body, 200)))?;
    value["choices"][0]["message"]["content"]
        .as_str()
        .map(str::to_owned)
        .ok_or_else(|| {
            anyhow::anyhow!(
                "resposta sem choices[0].message.content: {}",
                truncate(&body, 300)
            )
        })
}

/// Uma chamada de ferramenta pedida pelo modelo.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    /// Os argumentos em JSON, como texto (formato da API).
    pub arguments: String,
}

/// Uma resposta do modelo quando ele pode chamar ferramentas.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct AssistantTurn {
    /// Texto (vazio quando ele só pediu ferramentas).
    pub content: String,
    pub tool_calls: Vec<ToolCall>,
    pub input_tokens: u64,
    pub output_tokens: u64,
    /// A mensagem do assistente como veio: volta ao histórico antes dos
    /// resultados das ferramentas, que a API exige nessa ordem.
    pub raw_message: serde_json::Value,
}

/// `{base_url}/chat/completions` com `tools`: o modelo responde texto ou pede
/// ferramentas. `messages` e `tools` já no formato da API.
pub async fn chat_with_tools(
    base_url: &str,
    api_key: Option<&str>,
    model: &str,
    messages: &[serde_json::Value],
    tools: &[serde_json::Value],
) -> Result<AssistantTurn> {
    let url = format!("{}/chat/completions", base_url.trim_end_matches('/'));
    let mut payload = serde_json::json!({
        "model": model,
        "messages": messages,
        "stream": false,
    });
    if !tools.is_empty() {
        payload["tools"] = serde_json::Value::Array(tools.to_vec());
        payload["tool_choice"] = serde_json::json!("auto");
    }
    let mut req = reqwest::Client::new().post(&url).json(&payload);
    if let Some(key) = api_key {
        req = req.bearer_auth(key);
    }
    let resp = req.send().await.with_context(|| format!("chamando {url}"))?;
    let status = resp.status();
    let body = resp.text().await.context("lendo corpo da resposta")?;
    if !status.is_success() {
        bail!("HTTP {status} de {url}: {}", truncate(&body, 300));
    }
    let value: serde_json::Value = serde_json::from_str(&body)
        .with_context(|| format!("resposta não é JSON: {}", truncate(&body, 200)))?;
    parse_assistant_turn(&value)
}

/// Lê `choices[0].message` (texto e/ou `tool_calls`) e `usage`.
pub fn parse_assistant_turn(value: &serde_json::Value) -> Result<AssistantTurn> {
    let message = value
        .pointer("/choices/0/message")
        .ok_or_else(|| anyhow::anyhow!("resposta sem choices[0].message: {}", truncate(&value.to_string(), 300)))?;
    let tool_calls = message
        .get("tool_calls")
        .and_then(serde_json::Value::as_array)
        .map(|calls| {
            calls
                .iter()
                .enumerate()
                .map(|(i, c)| {
                    let f = c.get("function").unwrap_or(&serde_json::Value::Null);
                    ToolCall {
                        id: c
                            .get("id")
                            .and_then(serde_json::Value::as_str)
                            .map(str::to_owned)
                            .unwrap_or_else(|| format!("call_{i}")),
                        name: f.get("name").and_then(serde_json::Value::as_str).unwrap_or("").to_owned(),
                        // Alguns servidores (Ollama) mandam objeto em vez de texto.
                        arguments: match f.get("arguments") {
                            Some(serde_json::Value::String(s)) => s.clone(),
                            Some(v) if !v.is_null() => v.to_string(),
                            _ => "{}".to_owned(),
                        },
                    }
                })
                .collect()
        })
        .unwrap_or_default();
    let n = |k: &str| value.pointer(&format!("/usage/{k}")).and_then(serde_json::Value::as_u64).unwrap_or(0);
    Ok(AssistantTurn {
        content: message
            .get("content")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("")
            .to_owned(),
        tool_calls,
        input_tokens: n("prompt_tokens"),
        output_tokens: n("completion_tokens"),
        raw_message: message.clone(),
    })
}

/// Pede a lista de modelos do provedor (`GET {base_url}/models`, convenção
/// da API compatível com OpenAI) e devolve os ids, na ordem que o provedor
/// mandou.
pub async fn list_models(base_url: &str, api_key: Option<&str>) -> Result<Vec<String>> {
    let url = format!("{}/models", base_url.trim_end_matches('/'));
    let mut req = reqwest::Client::new().get(&url);
    if let Some(key) = api_key {
        req = req.bearer_auth(key);
    }
    let resp = req.send().await.with_context(|| format!("chamando {url}"))?;
    let status = resp.status();
    let body = resp.text().await.context("lendo corpo da resposta")?;
    if !status.is_success() {
        bail!("HTTP {status} de {url}: {}", truncate(&body, 300));
    }
    let value: serde_json::Value = serde_json::from_str(&body)
        .with_context(|| format!("resposta não é JSON: {}", truncate(&body, 200)))?;
    let ids = value
        .get("data")
        .and_then(serde_json::Value::as_array)
        .map(|models| {
            models
                .iter()
                .filter_map(|m| m.get("id").and_then(serde_json::Value::as_str))
                .map(str::to_owned)
                .collect()
        })
        .ok_or_else(|| {
            anyhow::anyhow!("resposta sem data[].id: {}", truncate(&body, 300))
        })?;
    Ok(ids)
}

/// Manda uma mensagem mínima (`.`) ao modelo e devolve o que ele respondeu, e
/// quanto levou — o botão "Testar" do seletor: prova que o modelo processa e
/// devolve algo, sem gastar contexto de verdade.
pub async fn probe_model(
    base_url: &str,
    api_key: Option<&str>,
    model: &str,
) -> Result<(String, std::time::Duration)> {
    let start = std::time::Instant::now();
    let text = chat_completion(base_url, api_key, model, &[ChatMessage::user(".".to_string())]).await?;
    Ok((text, start.elapsed()))
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let cut: String = s.chars().take(max).collect();
        format!("{cut}…")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn message_constructors() {
        assert_eq!(ChatMessage::system("a").role, "system");
        assert_eq!(ChatMessage::user("b").role, "user");
        assert_eq!(ChatMessage::assistant("c").role, "assistant");
    }

    #[test]
    fn reads_tool_calls_text_and_usage() {
        let v = serde_json::json!({
            "choices": [{"message": {
                "role": "assistant",
                "content": null,
                "tool_calls": [
                    {"id": "call_a", "type": "function", "function": {"name": "cli_start", "arguments": "{\"name\":\"api\"}"}},
                    {"type": "function", "function": {"name": "cli_status", "arguments": {"name": "api"}}}
                ]
            }}],
            "usage": {"prompt_tokens": 120, "completion_tokens": 9}
        });
        let t = parse_assistant_turn(&v).unwrap();
        assert_eq!(t.content, "");
        assert_eq!(t.tool_calls.len(), 2);
        assert_eq!(t.tool_calls[0], ToolCall { id: "call_a".into(), name: "cli_start".into(), arguments: "{\"name\":\"api\"}".into() });
        // Sem id: um estável; argumentos em objeto viram texto.
        assert_eq!(t.tool_calls[1].id, "call_1");
        assert_eq!(t.tool_calls[1].arguments, "{\"name\":\"api\"}");
        assert_eq!((t.input_tokens, t.output_tokens), (120, 9));
        assert_eq!(t.raw_message["tool_calls"][0]["id"], "call_a");

        let texto = serde_json::json!({"choices": [{"message": {"role": "assistant", "content": "Pronto."}}]});
        let t = parse_assistant_turn(&texto).unwrap();
        assert_eq!(t.content, "Pronto.");
        assert!(t.tool_calls.is_empty());
        assert!(parse_assistant_turn(&serde_json::json!({"error": "x"})).is_err());
    }

    fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
        haystack.windows(needle.len()).position(|w| w == needle)
    }

    /// Servidor compatível com OpenAI de mentira: devolve um pedido de tool e
    /// entrega o que recebeu, para conferir o que foi mandado.
    #[tokio::test]
    async fn sends_the_tools_and_reads_the_tool_calls_back() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let servidor = tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut buf = Vec::new();
            let mut pedaco = [0u8; 4096];
            loop {
                let n = sock.read(&mut pedaco).await.unwrap();
                if n == 0 {
                    break;
                }
                buf.extend_from_slice(&pedaco[..n]);
                if let Some(fim) = find(&buf, b"\r\n\r\n") {
                    let cabecalho = String::from_utf8_lossy(&buf[..fim]).to_lowercase();
                    let tamanho = cabecalho
                        .lines()
                        .find_map(|l| l.strip_prefix("content-length:"))
                        .and_then(|v| v.trim().parse::<usize>().ok())
                        .unwrap_or(0);
                    if buf.len() >= fim + 4 + tamanho {
                        break;
                    }
                }
            }
            let corpo = r#"{"choices":[{"message":{"role":"assistant","content":null,"tool_calls":[{"id":"call_1","type":"function","function":{"name":"cli_start","arguments":"{\"name\":\"api\"}"}}]}}],"usage":{"prompt_tokens":50,"completion_tokens":4}}"#;
            let resposta = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{corpo}",
                corpo.len()
            );
            sock.write_all(resposta.as_bytes()).await.unwrap();
            String::from_utf8_lossy(&buf).to_string()
        });
        let tools = vec![serde_json::json!({"type": "function", "function": {"name": "cli_start", "parameters": {"type": "object"}}})];
        let mensagens = vec![serde_json::json!({"role": "user", "content": "abra uma CLI api"})];
        let turno = chat_with_tools(&format!("http://{addr}/v1/"), Some("chave-teste"), "llama", &mensagens, &tools)
            .await
            .unwrap();
        let pedido = servidor.await.unwrap();
        assert!(pedido.starts_with("POST /v1/chat/completions"), "{pedido}");
        assert!(pedido.to_lowercase().contains("authorization: bearer chave-teste"));
        assert!(pedido.contains(r#""tool_choice":"auto""#) && pedido.contains("cli_start"), "{pedido}");
        assert_eq!(turno.tool_calls[0].name, "cli_start");
        assert_eq!(turno.tool_calls[0].arguments, r#"{"name":"api"}"#);
        assert_eq!((turno.input_tokens, turno.output_tokens), (50, 4));
    }

    /// Um servidor HTTP de mentira que devolve `resposta` em toda requisição
    /// e conta quantas chegaram, para testar `list_models`/`probe_model` sem
    /// bater na rede.
    async fn fake_server(
        resposta: &'static str,
    ) -> (std::net::SocketAddr, tokio::task::JoinHandle<Vec<u8>>) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut buf = Vec::new();
            let mut pedaco = [0u8; 4096];
            loop {
                let n = sock.read(&mut pedaco).await.unwrap();
                if n == 0 {
                    break;
                }
                buf.extend_from_slice(&pedaco[..n]);
                if buf.windows(4).any(|w| w == b"\r\n\r\n") {
                    break; // sem corpo (GET) ou corpo pequeno; basta para o teste
                }
            }
            let corpo = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{resposta}",
                resposta.len()
            );
            sock.write_all(corpo.as_bytes()).await.unwrap();
            buf
        });
        (addr, handle)
    }

    #[tokio::test]
    async fn list_models_reads_the_ids_in_order() {
        let (addr, pedido) = fake_server(
            r#"{"object":"list","data":[{"id":"llama-3.3-70b-versatile"},{"id":"openai/gpt-oss-120b"}]}"#,
        )
        .await;
        let ids = list_models(&format!("http://{addr}/v1"), Some("chave")).await.unwrap();
        assert_eq!(ids, vec!["llama-3.3-70b-versatile", "openai/gpt-oss-120b"]);
        let pedido = String::from_utf8_lossy(&pedido.await.unwrap()).to_string();
        assert!(pedido.starts_with("GET /v1/models"), "{pedido}");
        assert!(pedido.to_lowercase().contains("authorization: bearer chave"));
    }

    #[tokio::test]
    async fn list_models_rejects_a_response_without_data() {
        let (addr, _) = fake_server(r#"{"erro":"sem data"}"#).await;
        assert!(list_models(&format!("http://{addr}/v1"), None).await.is_err());
    }

    #[tokio::test]
    async fn probe_model_returns_the_reply_and_measures_time() {
        let (addr, pedido) = fake_server(
            r#"{"choices":[{"message":{"role":"assistant","content":"ok"}}]}"#,
        )
        .await;
        let (texto, duracao) = probe_model(&format!("http://{addr}/v1"), None, "algum-modelo")
            .await
            .unwrap();
        assert_eq!(texto, "ok");
        assert!(duracao.as_millis() < 5_000);
        let pedido = String::from_utf8_lossy(&pedido.await.unwrap()).to_string();
        assert!(pedido.contains(r#""model":"algum-modelo""#), "{pedido}");
        assert!(pedido.contains(r#""content":".""#), "{pedido}");
    }

    #[test]
    fn truncate_respects_utf8() {
        assert_eq!(truncate("çãé", 2), "çã…");
        assert_eq!(truncate("ok", 10), "ok");
    }
}
