//! Cliente mínimo do Chrome DevTools Protocol.
//!
//! Só o necessário para o orquestrador testar: abrir URL, rodar JavaScript
//! (é assim que lemos os elementos e agimos neles), ler o console e tirar
//! screenshot. Síncrono de propósito — cada chamada é um passo do teste, e
//! sincronismo deixa o erro fácil de ler.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use tungstenite::{connect, Message, WebSocket};

/// Tempo máximo esperando o navegador responder a um comando.
const CALL_TIMEOUT: Duration = Duration::from_secs(30);

/// Conexão com uma aba do navegador da sandbox.
pub struct Cdp {
    socket: WebSocket<tungstenite::stream::MaybeTlsStream<TcpStream>>,
    next_id: u64,
    /// Mensagens do console acumuladas desde a conexão.
    console: Vec<String>,
}

/// Descobre a aba aberta e devolve o endereço WebSocket dela.
///
/// O `/json` do DevTools é HTTP simples; fazemos a requisição na unha para
/// não arrastar um cliente HTTP inteiro como dependência.
pub fn target_ws_url(cdp_http: &str) -> Result<String> {
    let url = url::Url::parse(cdp_http).context("endereço do DevTools inválido")?;
    let host = url.host_str().unwrap_or("127.0.0.1");
    let port = url.port().unwrap_or(9222);
    let mut stream = TcpStream::connect((host, port))
        .with_context(|| format!("o navegador da sandbox não respondeu em {host}:{port}"))?;
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    write!(
        stream,
        "GET /json/list HTTP/1.1\r\nHost: {host}:{port}\r\nConnection: close\r\n\r\n"
    )?;
    // Lemos em pedaços em vez de `read_to_string`: com timeout de leitura,
    // ele devolve erro e JOGA FORA o que já tinha lido quando o servidor
    // demora a fechar a conexão.
    let mut raw: Vec<u8> = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        match stream.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => {
                raw.extend_from_slice(&chunk[..n]);
                // Corpo JSON completo? Não precisa esperar o fechamento.
                if response_is_complete(&raw) {
                    break;
                }
            }
            Err(_) => break,
        }
    }
    let body = String::from_utf8_lossy(&raw).into_owned();
    let json_start = body
        .find("\r\n\r\n")
        .map(|i| i + 4)
        .context("o DevTools não respondeu (o navegador ainda está subindo?)")?;
    parse_ws_url(&body[json_start..])
}

/// A resposta HTTP já tem o corpo inteiro? Usa `Content-Length` quando
/// existe; senão, tenta fechar o JSON.
fn response_is_complete(raw: &[u8]) -> bool {
    let text = String::from_utf8_lossy(raw);
    let Some(head_end) = text.find("\r\n\r\n") else {
        return false;
    };
    let body = &text[head_end + 4..];
    if let Some(len) = content_length(&text[..head_end]) {
        return body.len() >= len;
    }
    let t = body.trim_end();
    !t.is_empty() && (t.ends_with(']') || t.ends_with('}'))
}

/// Lê o `Content-Length` do cabeçalho, se houver.
fn content_length(head: &str) -> Option<usize> {
    head.lines()
        .find(|l| l.to_ascii_lowercase().starts_with("content-length:"))
        .and_then(|l| l.split(':').nth(1))
        .and_then(|v| v.trim().parse().ok())
}

/// Extrai o `webSocketDebuggerUrl` da primeira aba de página.
pub fn parse_ws_url(body: &str) -> Result<String> {
    let targets: Value = serde_json::from_str(body.trim())
        .context("não consegui ler a lista de abas do DevTools")?;
    let list = targets.as_array().context("lista de abas inesperada")?;
    list.iter()
        .find(|t| t.get("type").and_then(Value::as_str) == Some("page"))
        .and_then(|t| t.get("webSocketDebuggerUrl"))
        .and_then(Value::as_str)
        .map(str::to_string)
        .context("nenhuma aba aberta no navegador da sandbox")
}

impl Cdp {
    /// Conecta na aba e liga os eventos que nos interessam.
    pub fn connect(cdp_http: &str) -> Result<Self> {
        let ws_url = target_ws_url(cdp_http)?;
        let (socket, _) = connect(&ws_url)
            .with_context(|| format!("conectando no DevTools em {ws_url}"))?;
        let mut cdp = Self {
            socket,
            next_id: 0,
            console: Vec::new(),
        };
        // Console e erros de página: o orquestrador precisa VER o que quebrou.
        cdp.call("Runtime.enable", json!({}))?;
        cdp.call("Log.enable", json!({}))?;
        cdp.call("Page.enable", json!({}))?;
        Ok(cdp)
    }

    /// Envia um comando e espera a resposta correspondente.
    pub fn call(&mut self, method: &str, params: Value) -> Result<Value> {
        self.next_id += 1;
        let id = self.next_id;
        let msg = json!({ "id": id, "method": method, "params": params });
        self.socket
            .send(Message::Text(msg.to_string()))
            .with_context(|| format!("enviando {method}"))?;

        let start = Instant::now();
        loop {
            if start.elapsed() > CALL_TIMEOUT {
                bail!("o navegador não respondeu a {method} em {CALL_TIMEOUT:?}");
            }
            let msg = match self.socket.read() {
                Ok(Message::Text(t)) => t,
                Ok(Message::Close(_)) => bail!("o navegador fechou a conexão"),
                Ok(_) => continue,
                Err(e) => bail!("erro lendo do navegador: {e}"),
            };
            let v: Value = match serde_json::from_str(&msg) {
                Ok(v) => v,
                Err(_) => continue,
            };
            // Evento: guarda o que for console/erro e segue esperando.
            if v.get("id").is_none() {
                if let Some(line) = console_line(&v) {
                    self.console.push(line);
                }
                continue;
            }
            if v.get("id").and_then(Value::as_u64) != Some(id) {
                continue;
            }
            if let Some(err) = v.get("error") {
                bail!(
                    "{method}: {}",
                    err.get("message").and_then(Value::as_str).unwrap_or("erro")
                );
            }
            return Ok(v.get("result").cloned().unwrap_or(Value::Null));
        }
    }

    /// Roda JavaScript na página e devolve o valor retornado como texto.
    pub fn eval(&mut self, script: &str) -> Result<String> {
        let result = self.call(
            "Runtime.evaluate",
            json!({
                "expression": script,
                "returnByValue": true,
                "awaitPromise": true,
            }),
        )?;
        if let Some(exc) = result.get("exceptionDetails") {
            let text = exc
                .get("exception")
                .and_then(|e| e.get("description"))
                .and_then(Value::as_str)
                .or_else(|| exc.get("text").and_then(Value::as_str))
                .unwrap_or("erro de JavaScript");
            bail!("{text}");
        }
        Ok(result
            .get("result")
            .and_then(|r| r.get("value"))
            .map(|v| match v {
                Value::String(s) => s.clone(),
                other => other.to_string(),
            })
            .unwrap_or_default())
    }

    /// Navega e espera a página carregar.
    pub fn navigate(&mut self, url: &str) -> Result<()> {
        self.call("Page.navigate", json!({ "url": url }))?;
        // Espera o `load`; se não vier, seguimos assim mesmo (SPA lenta).
        let start = Instant::now();
        while start.elapsed() < Duration::from_secs(15) {
            if self.eval("document.readyState").unwrap_or_default() == "complete" {
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(150));
        }
        Ok(())
    }

    /// PNG da tela, em base64.
    pub fn screenshot(&mut self) -> Result<String> {
        let r = self.call(
            "Page.captureScreenshot",
            json!({ "format": "png", "captureBeyondViewport": false }),
        )?;
        r.get("data")
            .and_then(Value::as_str)
            .map(str::to_string)
            .context("o navegador não devolveu a imagem")
    }

    /// Mensagens de console acumuladas (e as esvazia).
    pub fn take_console(&mut self) -> Vec<String> {
        std::mem::take(&mut self.console)
    }

    /// Drena eventos pendentes sem bloquear — pega o console que chegou
    /// depois da última chamada.
    pub fn pump(&mut self) {
        let _ = self.call("Runtime.evaluate", json!({ "expression": "1" }));
    }
}

/// Traduz um evento do DevTools numa linha de console legível.
pub fn console_line(event: &Value) -> Option<String> {
    match event.get("method").and_then(Value::as_str)? {
        "Runtime.consoleAPICalled" => {
            let p = event.get("params")?;
            let level = p.get("type").and_then(Value::as_str).unwrap_or("log");
            let args: Vec<String> = p
                .get("args")?
                .as_array()?
                .iter()
                .map(|a| {
                    a.get("value")
                        .map(|v| match v {
                            Value::String(s) => s.clone(),
                            other => other.to_string(),
                        })
                        .or_else(|| {
                            a.get("description").and_then(Value::as_str).map(str::to_string)
                        })
                        .unwrap_or_default()
                })
                .collect();
            Some(format!("[{level}] {}", args.join(" ")))
        }
        "Runtime.exceptionThrown" => {
            let d = event.get("params")?.get("exceptionDetails")?;
            let text = d
                .get("exception")
                .and_then(|e| e.get("description"))
                .and_then(Value::as_str)
                .or_else(|| d.get("text").and_then(Value::as_str))
                .unwrap_or("exceção");
            Some(format!("[erro] {text}"))
        }
        "Log.entryAdded" => {
            let e = event.get("params")?.get("entry")?;
            let level = e.get("level").and_then(Value::as_str).unwrap_or("info");
            let text = e.get("text").and_then(Value::as_str).unwrap_or("");
            Some(format!("[{level}] {text}"))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_the_page_target_among_others() {
        let body = r#"[
            {"type":"background_page","webSocketDebuggerUrl":"ws://x/bg"},
            {"type":"page","webSocketDebuggerUrl":"ws://127.0.0.1:9222/devtools/page/ABC"}
        ]"#;
        assert_eq!(
            parse_ws_url(body).unwrap(),
            "ws://127.0.0.1:9222/devtools/page/ABC"
        );
    }

    #[test]
    fn no_page_target_is_a_clear_error() {
        let body = r#"[{"type":"worker","webSocketDebuggerUrl":"ws://x/w"}]"#;
        let err = parse_ws_url(body).unwrap_err().to_string();
        assert!(err.contains("nenhuma aba"), "{err}");
        assert!(parse_ws_url("não é json").is_err());
    }

    #[test]
    fn console_events_become_readable_lines() {
        let ev = json!({
            "method": "Runtime.consoleAPICalled",
            "params": { "type": "error", "args": [{"value": "falhou"}, {"value": 42}] }
        });
        assert_eq!(console_line(&ev).unwrap(), "[error] falhou 42");

        let ev = json!({
            "method": "Runtime.exceptionThrown",
            "params": { "exceptionDetails": { "exception": { "description": "TypeError: x" } } }
        });
        assert_eq!(console_line(&ev).unwrap(), "[erro] TypeError: x");

        let ev = json!({
            "method": "Log.entryAdded",
            "params": { "entry": { "level": "warning", "text": "404 /favicon.ico" } }
        });
        assert_eq!(console_line(&ev).unwrap(), "[warning] 404 /favicon.ico");

        // Evento que não interessa não vira linha.
        assert!(console_line(&json!({ "method": "Page.frameNavigated" })).is_none());
    }
}
