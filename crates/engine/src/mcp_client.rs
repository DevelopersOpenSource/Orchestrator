//! Cliente MCP por stdio, para o chat HTTP usar as tools do Orchestrator.
//!
//! Claude Code, Codex, Kimi e OpenCode sobem o `orchestrator-mcp` sozinhos. O
//! chat HTTP (Groq, OpenRouter, Ollama, NVIDIA) não tem quem faça isso: este
//! cliente sobe o mesmo servidor, em modo trava, lista as tools e as chama
//! quando o modelo pede. A decisão de cada chamada (regras do dono, consulta
//! à memória antes de mudar algo) acontece dentro do servidor.

use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use serde_json::{json, Value};

pub struct McpClient<W: Write, R: BufRead> {
    writer: Option<W>,
    reader: R,
    next_id: u64,
    /// O processo que este cliente abriu, quando foi ele que abriu.
    child: Option<Child>,
}

/// Cliente ligado a um `orchestrator-mcp` que ele mesmo subiu.
pub type SpawnedMcp = McpClient<ChildStdin, BufReader<ChildStdout>>;

impl SpawnedMcp {
    pub fn spawn(command: &Path, env: &[(String, String)], dir: &Path) -> Result<Self> {
        let mut child = Command::new(command)
            .envs(env.iter().map(|(k, v)| (k, v)))
            .current_dir(dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .with_context(|| format!("subindo {}", command.display()))?;
        let stdin = child.stdin.take().context("stdin do servidor MCP")?;
        let stdout = child.stdout.take().context("stdout do servidor MCP")?;
        let mut client = McpClient {
            writer: Some(stdin),
            reader: BufReader::new(stdout),
            next_id: 1,
            child: Some(child),
        };
        client.initialize()?;
        Ok(client)
    }
}

impl<W: Write, R: BufRead> McpClient<W, R> {
    pub fn new(writer: W, reader: R) -> Self {
        Self {
            writer: Some(writer),
            reader,
            next_id: 1,
            child: None,
        }
    }

    pub fn initialize(&mut self) -> Result<Value> {
        let resultado = self.request(
            "initialize",
            json!({
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": { "name": "orchestrator-chat", "version": env!("CARGO_PKG_VERSION") }
            }),
        )?;
        self.send(&json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }))?;
        Ok(resultado)
    }

    fn send(&mut self, v: &Value) -> Result<()> {
        let w = self.writer.as_mut().context("conexão com o servidor MCP fechada")?;
        writeln!(w, "{v}")?;
        w.flush()?;
        Ok(())
    }

    /// Uma requisição e a resposta de mesmo id.
    pub fn request(&mut self, method: &str, params: Value) -> Result<Value> {
        let id = self.next_id;
        self.next_id += 1;
        self.send(&json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }))?;
        let mut linha = String::new();
        loop {
            linha.clear();
            if self.reader.read_line(&mut linha)? == 0 {
                bail!("o servidor MCP fechou a conexão");
            }
            let Ok(v) = serde_json::from_str::<Value>(linha.trim()) else {
                continue;
            };
            if v.get("id").and_then(Value::as_u64) != Some(id) {
                continue;
            }
            if let Some(e) = v.get("error") {
                bail!(
                    "{}",
                    e.get("message").and_then(Value::as_str).unwrap_or("erro do servidor MCP")
                );
            }
            return Ok(v.get("result").cloned().unwrap_or(Value::Null));
        }
    }

    /// As tools como o servidor as descreve (`name`, `description`, `inputSchema`).
    pub fn list_tools(&mut self) -> Result<Vec<Value>> {
        Ok(self
            .request("tools/list", json!({}))?
            .get("tools")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default())
    }

    /// Chama uma tool e devolve o texto para o modelo. Bloqueio ou falha da
    /// tool (`isError`) volta como texto marcado, para o modelo reagir.
    pub fn call_tool(&mut self, name: &str, arguments: &Value) -> Result<String> {
        let r = self.request("tools/call", json!({ "name": name, "arguments": arguments }))?;
        let texto = r
            .get("content")
            .and_then(Value::as_array)
            .map(|partes| {
                partes
                    .iter()
                    .filter_map(|p| p.get("text").and_then(Value::as_str))
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .unwrap_or_default();
        if r.get("isError").and_then(Value::as_bool) == Some(true) {
            Ok(format!("ERRO: {texto}"))
        } else {
            Ok(texto)
        }
    }
}

impl<W: Write, R: BufRead> Drop for McpClient<W, R> {
    fn drop(&mut self) {
        // Fim da entrada: o servidor sai do laço sozinho.
        self.writer.take();
        let Some(mut child) = self.child.take() else {
            return;
        };
        let limite = Instant::now() + Duration::from_secs(2);
        while Instant::now() < limite {
            if matches!(child.try_wait(), Ok(Some(_))) {
                return;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        // Só o processo que este cliente abriu, pelo próprio handle.
        let _ = child.kill();
        let _ = child.wait();
    }
}

/// As tools MCP no formato `tools` da API de chat compatível com OpenAI.
pub fn openai_functions(tools: &[Value]) -> Vec<Value> {
    tools
        .iter()
        .filter_map(|t| {
            let name = t.get("name")?.as_str()?;
            // Contrato do `--permission-prompt-tool` do Claude Code, não é
            // para o modelo chamar.
            if name == "permission_prompt" {
                return None;
            }
            Some(json!({
                "type": "function",
                "function": {
                    "name": name,
                    "description": t.get("description").and_then(Value::as_str).unwrap_or(""),
                    "parameters": t
                        .get("inputSchema")
                        .cloned()
                        .unwrap_or_else(|| json!({ "type": "object", "properties": {} })),
                }
            }))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Servidor MCP de mentira numa thread, falando por pipes.
    fn fake_server() -> McpClient<std::io::PipeWriter, BufReader<std::io::PipeReader>> {
        let (pedidos_r, pedidos_w) = std::io::pipe().unwrap();
        let (respostas_r, mut respostas_w) = std::io::pipe().unwrap();
        std::thread::spawn(move || {
            for linha in BufReader::new(pedidos_r).lines() {
                let Ok(linha) = linha else { break };
                let v: Value = serde_json::from_str(&linha).unwrap();
                let Some(id) = v.get("id").cloned() else { continue };
                let resposta = match v["method"].as_str().unwrap() {
                    "initialize" => json!({ "jsonrpc": "2.0", "id": id, "result": { "protocolVersion": "2024-11-05" } }),
                    "tools/list" => json!({ "jsonrpc": "2.0", "id": id, "result": { "tools": [
                        { "name": "cli_start", "description": "abre uma CLI", "inputSchema": { "type": "object", "properties": { "name": { "type": "string" } } } },
                        { "name": "permission_prompt", "description": "interno" }
                    ]}}),
                    "tools/call" => match v["params"]["name"].as_str().unwrap() {
                        "cli_start" => json!({ "jsonrpc": "2.0", "id": id, "result": { "content": [{ "type": "text", "text": format!("CLI {} aberta", v["params"]["arguments"]["name"].as_str().unwrap()) }] } }),
                        "cli_send" => json!({ "jsonrpc": "2.0", "id": id, "result": { "content": [{ "type": "text", "text": "Bloqueado pelo Orchestrator: consulte a memória" }], "isError": true } }),
                        _ => json!({ "jsonrpc": "2.0", "id": id, "error": { "code": -32602, "message": "tool desconhecida" } }),
                    },
                    _ => continue,
                };
                writeln!(respostas_w, "{resposta}").unwrap();
            }
        });
        McpClient::new(pedidos_w, BufReader::new(respostas_r))
    }

    #[test]
    fn lists_and_calls_tools_and_reports_blocks_as_text() {
        let mut c = fake_server();
        c.initialize().unwrap();
        let tools = c.list_tools().unwrap();
        assert_eq!(tools.len(), 2);
        assert_eq!(c.call_tool("cli_start", &json!({ "name": "api" })).unwrap(), "CLI api aberta");
        let bloqueio = c.call_tool("cli_send", &json!({})).unwrap();
        assert!(bloqueio.starts_with("ERRO: Bloqueado"), "{bloqueio}");
        let erro = c.call_tool("nada", &json!({})).unwrap_err();
        assert!(erro.to_string().contains("tool desconhecida"));
    }

    #[test]
    fn functions_keep_the_schema_and_skip_the_internal_tool() {
        let f = openai_functions(&[
            json!({ "name": "cli_start", "description": "abre", "inputSchema": { "type": "object", "required": ["name"] } }),
            json!({ "name": "permission_prompt" }),
            json!({ "name": "cli_status" }),
        ]);
        assert_eq!(f.len(), 2);
        assert_eq!(f[0]["function"]["name"], "cli_start");
        assert_eq!(f[0]["function"]["parameters"]["required"][0], "name");
        assert_eq!(f[1]["function"]["parameters"]["type"], "object");
    }
}
