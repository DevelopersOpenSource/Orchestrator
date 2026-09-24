//! Dispatch JSON-RPC 2.0 do protocolo MCP sobre o `MemoryStore`.
//!
//! Implementa o subconjunto necessário para expor a memória como tools:
//! `initialize`, `tools/list` e `tools/call`. Notificações (sem `id`)
//! não produzem resposta, conforme a especificação.

use std::str::FromStr;
use std::time::{Duration, Instant};

use orchestrator_memory::store::MemoryStore;
use orchestrator_memory::MemoryKind;
use serde_json::{json, Value};

use crate::enforce::{evaluate, Verdict};
use crate::gate;

const PROTOCOL_VERSION: &str = "2024-11-05";

/// Timeout padrão (segundos) que a tool `permission_prompt` espera por uma
/// decisão do usuário antes de negar (fail-closed). Sobreponível por
/// `ORCHESTRATOR_DECISION_TIMEOUT_SECS`.
const DEFAULT_DECISION_TIMEOUT_SECS: u64 = 120;

/// Timeout (segundos) que as tools `cli_*` esperam pela TUI confirmar a
/// execução do comando. Curto de propósito: se a TUI não está rodando, o
/// orquestrador precisa saber logo, e não travar o turno. Sobreponível por
/// `ORCHESTRATOR_CLI_ACK_TIMEOUT_SECS`.
const DEFAULT_CLI_ACK_TIMEOUT_SECS: u64 = 15;

fn cli_ack_timeout_secs() -> u64 {
    std::env::var("ORCHESTRATOR_CLI_ACK_TIMEOUT_SECS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .filter(|s| *s > 0)
        .unwrap_or(DEFAULT_CLI_ACK_TIMEOUT_SECS)
}

/// Linhas do fim da tela que `cli_status` devolve por padrão. Curto de
/// propósito: a tela inteira em toda consulta é o que estourava o contexto.
const STATUS_LINES: usize = 12;

/// Trava aplicada dentro do servidor, para ferramentas de IA sem hook
/// compatível (OpenCode) e para o chat HTTP. Ligada por
/// `ORCHESTRATOR_GATE_IN_MCP=1`, que quem abre a ferramenta exporta.
#[derive(Debug, Clone)]
pub struct McpGate {
    /// Sessão do chat (`ORCHESTRATOR_SESSION`): a consulta à memória vale
    /// para ela inteira, não para cada processo do servidor.
    pub session_id: String,
    pub options: gate::Options,
}

impl McpGate {
    pub fn from_env() -> Option<Self> {
        if std::env::var("ORCHESTRATOR_GATE_IN_MCP").ok().as_deref() != Some("1") {
            return None;
        }
        Some(Self {
            session_id: std::env::var("ORCHESTRATOR_SESSION")
                .ok()
                .filter(|s| !s.trim().is_empty())
                .unwrap_or_else(|| format!("mcp-{}", std::process::id())),
            options: gate::Options {
                autonomous: std::env::var("ORCHESTRATOR_AUTONOMOUS").ok().as_deref() == Some("1"),
                notify: true,
                requester: std::env::var("ORCHESTRATOR_AGENT").ok(),
            },
        })
    }
}

/// Processa uma linha de requisição JSON-RPC; `None` = sem resposta
/// (notificação ou linha inválida sem id).
pub fn handle_line(store: &MemoryStore, line: &str) -> Option<String> {
    handle_line_with(store, line, McpGate::from_env().as_ref())
}

/// [`handle_line`] com a trava explícita (os testes não mexem no ambiente).
pub fn handle_line_with(store: &MemoryStore, line: &str, gate: Option<&McpGate>) -> Option<String> {
    let req: Value = match serde_json::from_str(line.trim()) {
        Ok(v) => v,
        Err(_) => return None,
    };
    let id = req.get("id").cloned();
    let method = req.get("method").and_then(Value::as_str).unwrap_or("");
    let params = req.get("params").cloned().unwrap_or(Value::Null);

    // Notificações não têm id e não recebem resposta.
    let id = match id {
        Some(id) if !id.is_null() => id,
        _ => return None,
    };

    let result = match method {
        "initialize" => Ok(json!({
            "protocolVersion": PROTOCOL_VERSION,
            "capabilities": { "tools": {} },
            "serverInfo": { "name": "orchestrator-mcp", "version": env!("CARGO_PKG_VERSION") }
        })),
        "ping" => Ok(json!({})),
        "tools/list" => Ok(tools_list()),
        "tools/call" => tools_call(store, &params, gate),
        _ => Err((-32601, format!("método desconhecido: {method}"))),
    };

    let response = match result {
        Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
        Err((code, message)) => json!({
            "jsonrpc": "2.0", "id": id,
            "error": { "code": code, "message": message }
        }),
    };
    Some(response.to_string())
}

fn tools_list() -> Value {
    let mut value = json!({
        "tools": [
            {
                "name": "retrieve_memory",
                "description": "Busca semântica na memória (ChromaDB + reranker multilíngue) sobre o que o projeto enxerga: regras do DONO do projeto, memórias GLOBAIS do dono (valem em todo projeto) e memórias das IAs, cada uma com autor. Cada resultado diz a origem. OBRIGATÓRIO antes de alterar qualquer coisa — ferramentas que alteram o projeto ficam bloqueadas até a primeira consulta da sessão.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "project": { "type": "string", "description": "Nome do projeto (padrão: o projeto atual)" },
                        "query": { "type": "string", "description": "O que você vai fazer ou quer saber" },
                        "kind": { "type": "string", "enum": ["security", "architecture", "practice", "syntax", "decision"], "description": "Filtro opcional por tipo" },
                        "top_k": { "type": "integer", "default": 5 }
                    },
                    "required": ["query"]
                }
            },
            {
                "name": "store_memory",
                "description": "Grava uma memória SUA (fica com o seu nome como autor; nunca vira regra do dono). Use para o que aprendeu e vale lembrar: convenção, decisão, armadilha, preferência do dono. Passa por remoção de segredos. Com global=true ela vale em TODO projeto — use para o que aprendeu sobre o DONO (preferências, jeito de trabalhar, o que ele cobra), não para detalhe deste código; só funciona se o dono liberou (/memoria-global on).",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "project": { "type": "string", "description": "Padrão: o projeto atual" },
                        "kind": { "type": "string", "enum": ["security", "architecture", "practice", "syntax", "decision"] },
                        "title": { "type": "string" },
                        "body": { "type": "string" },
                        "priority": { "type": "integer", "default": 0 },
                        "global": { "type": "boolean", "description": "Vale em todo projeto (preferência do dono). Padrão: false." }
                    },
                    "required": ["kind", "title", "body"]
                }
            },
            {
                "name": "ssh_exec",
                "description": "Roda um comando num servidor (VPS) que o DONO cadastrou para este projeto (Conexões SSH no app), com a chave dele e sem pedir senha. Sem `host`, lista os servidores cadastrados. O comando roda com o usuário cadastrado (muitas vezes root): seja cuidadoso, prefira comandos de leitura antes de mudar algo, e não rode nada destrutivo sem o dono ter pedido. Prazo de 120s por comando.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "host": { "type": "string", "description": "Nome do servidor como o dono cadastrou." },
                        "command": { "type": "string", "description": "Comando shell a rodar lá." }
                    },
                    "required": []
                }
            },
            {
                "name": "list_memories",
                "description": "Lista o que o projeto enxerga — as memórias dele e as globais do dono — com a origem de cada uma (opcionalmente por tipo).",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "project": { "type": "string", "description": "Padrão: o projeto atual" },
                        "kind": { "type": "string", "enum": ["security", "architecture", "practice", "syntax", "decision"] }
                    },
                    "required": []
                }
            },
            {
                "name": "log_decision",
                "description": "Registra uma decisão no log de auditoria append-only do projeto.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "project": { "type": "string" },
                        "session_id": { "type": "string" },
                        "action": { "type": "string" },
                        "decision": { "type": "string" },
                        "reason": { "type": "string" }
                    },
                    "required": ["project", "session_id", "action", "decision", "reason"]
                }
            },
            {
                "name": "permission_prompt",
                "description": "Ferramenta de permissão do Orchestrator (usada via --permission-prompt-tool). Avalia a tool call contra as regras de segurança e, quando preciso, pausa aguardando a decisão do usuário na TUI. Responde no contrato do Claude Code: {\"behavior\":\"allow\",\"updatedInput\":...} ou {\"behavior\":\"deny\",\"message\":...}.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "tool_name": { "type": "string" },
                        "input": { "type": "object" },
                        "session_id": { "type": "string" },
                        "project": { "type": "string" }
                    },
                    "required": ["tool_name", "input"]
                }
            },
            {
                "name": "cli_start",
                "description": "Abre uma CLI REAL (terminal interativo) como card na workspace do Orchestrator, com o nome que você escolher (ex.: \"frontend\", \"backend\", \"testes\"). É assim que você delega trabalho: você NÃO escreve código nem edita arquivos — você comanda CLIs. Devolve confirmação quando a TUI abriu o terminal.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "name": { "type": "string", "description": "Nome da CLI (único na workspace), ex.: \"frontend\"." },
                        "command": { "type": "string", "description": "Binário a rodar (default: \"claude\"). Outras CLIs de agente: \"codex\" (ChatGPT), \"kimi\" (Kimi Code), \"agy\" (Google Antigravity), \"opencode\"; ou o nome de uma CLI cadastrada, ex.: \"Claude Code · GLM\"." },
                        "args": { "type": "array", "items": { "type": "string" }, "description": "Argumentos extras do binário." },
                        "cwd": { "type": "string", "description": "Pasta de trabalho (default: a pasta da workspace/projeto)." }
                    },
                    "required": ["name"]
                }
            },
            {
                "name": "cli_send",
                "description": "Envia um prompt (tarefa/instrução) para uma CLI já aberta, como se você tivesse digitado nela. Volta assim que o prompt é entregue — NÃO espera a tarefa terminar: quando a CLI concluir, você recebe automaticamente uma notificação de sistema no chat dizendo que ela terminou, e então usa cli_status para ver o resultado e decidir o próximo passo.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "name": { "type": "string", "description": "Nome da CLI destino." },
                        "prompt": { "type": "string", "description": "O que a CLI deve fazer (uma tarefa concreta por envio)." }
                    },
                    "required": ["name", "prompt"]
                }
            },
            {
                "name": "cli_status",
                "description": "Situação das CLIs abertas (starting/working/idle/exited) e um RESUMO curto do fim da tela. É de propósito curto para não gastar contexto: quando precisar de mais, use cli_read (que lê o histórico inteiro e busca dentro dele). Sem \"name\" lista todas as CLIs.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "name": { "type": "string", "description": "Nome de uma CLI específica (omita para listar todas)." },
                        "lines": { "type": "integer", "description": "Linhas do fim da tela a incluir (padrão 12, máximo 60)." }
                    }
                }
            },
            {
                "name": "cli_read",
                "description": "Lê o HISTÓRICO de uma CLI — inclusive o que já saiu da tela. Sem \"search\", devolve as últimas \"lines\" linhas; com \"search\", devolve só os trechos que contêm o termo (com algumas linhas de contexto em volta). Use isto em vez de pedir a tela inteira: procure o que interessa (ex.: \"error\", \"FAILED\", o nome de um arquivo) em vez de trazer tudo.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "name": { "type": "string", "description": "Nome da CLI." },
                        "search": { "type": "string", "description": "Termo a procurar no histórico (ignora maiúsculas)." },
                        "lines": { "type": "integer", "description": "Sem busca: quantas linhas do fim (padrão 40, máximo 400)." },
                        "context": { "type": "integer", "description": "Com busca: linhas de contexto ao redor de cada achado (padrão 3)." }
                    },
                    "required": ["name"]
                }
            },
            {
                "name": "cli_menu",
                "description": "Lê a tela da CLI como um MENU DE ESCOLHA: quais opções existem, qual está SELECIONADA agora e quais aceitam resposta escrita. Use sempre que a CLI fizer uma pergunta com alternativas (confirmação, questionário, seleção de arquivo) — o texto puro de cli_status mostra as alternativas mas não diz onde está o cursor.",
                "inputSchema": {
                    "type": "object",
                    "properties": { "name": { "type": "string" } },
                    "required": ["name"]
                }
            },
            {
                "name": "cli_choose",
                "description": "ESCOLHE uma opção do menu que está na tela: navega com as setas a partir de onde o cursor está e confirma com Enter. Identifique a opção pelo texto (\"Não usar\"), por parte dele (\"não\") ou pelo número (\"3\"). Se a opção pedir uma resposta escrita (\"4 - Outra resposta\"), mande também o campo `resposta` com o texto — ela é selecionada e o texto digitado em seguida.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "name": { "type": "string", "description": "Nome da CLI." },
                        "opcao": { "type": "string", "description": "Texto, trecho ou número da opção desejada." },
                        "resposta": { "type": "string", "description": "Texto a escrever quando a opção for de resposta livre." }
                    },
                    "required": ["name", "opcao"]
                }
            },
            {
                "name": "cli_key",
                "description": "Manda uma tecla crua para a CLI: up, down, left, right, enter, esc, tab, space, backspace. Use para casos que cli_choose não cobre (fechar um diálogo com esc, marcar caixa com space, avançar campo com tab).",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "name": { "type": "string" },
                        "tecla": { "type": "string", "description": "up|down|left|right|enter|esc|tab|space|backspace" },
                        "vezes": { "type": "integer", "description": "Repetições (padrão 1, máximo 20)." }
                    },
                    "required": ["name", "tecla"]
                }
            },
            {
                "name": "cli_stop",
                "description": "Encerra uma CLI aberta (mata o processo e fecha o card). Use quando o trabalho dela acabou ou travou.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "name": { "type": "string", "description": "Nome da CLI a encerrar." }
                    },
                    "required": ["name"]
                }
            },
            {
                "name": "decision_resolve",
                "description": "MODO AUTÔNOMO: decide um pedido que uma CLI do workspace fez e que uma regra de segurança mandou confirmar. Esses pedidos chegam a você como mensagem de sistema, com o id. Aprove quando a ação bate com o que o dono pediu e é segura para o projeto; negue com o motivo quando não bate. Se for MUITO crítico (irreversível, fora do escopo), fugir do pedido, ou você não souber decidir, use decision_escalate. Depois avise a CLI com cli_send.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "id": { "type": "string", "description": "Id da decisão (basta o começo, 8 caracteres)." },
                        "approve": { "type": "boolean", "description": "true aprova, false nega." },
                        "reason": { "type": "string", "description": "Por que, em uma frase: fica na auditoria e volta para a CLI." }
                    },
                    "required": ["id", "approve", "reason"]
                }
            },
            {
                "name": "decision_escalate",
                "description": "Passa ao dono um pedido que seria seu: quando a ação é MUITO crítica, não bate com o que ele pediu, ou você não sabe decidir. Ele responde no app ou na TUI; a resposta chega como mensagem de sistema. Siga outra frente enquanto isso.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "id": { "type": "string", "description": "Id da decisão (basta o começo, 8 caracteres)." },
                        "reason": { "type": "string", "description": "O que o dono precisa saber para decidir." }
                    },
                    "required": ["id", "reason"]
                }
            },
            {
                "name": "ask_owner",
                "description": "Pergunta ao dono quando a escolha é DELE (escopo, arquitetura, preferência) e você não pode decidir sozinho. Dê alternativas curtas em options; multiple=true deixa marcar várias; sem options a resposta é livre. Não trava: siga outra frente, a resposta chega como mensagem de sistema. Não use para o que você mesmo pode decidir.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "question": { "type": "string", "description": "A pergunta, curta e com o contexto necessário." },
                        "options": { "type": "array", "items": { "type": "string" }, "description": "Alternativas (opcional)." },
                        "multiple": { "type": "boolean", "description": "Aceita mais de uma alternativa (padrão: false)." }
                    },
                    "required": ["question"]
                }
            }
        ]
    });
    // As tools de sandbox moram em `ui.rs` (elas agem no container, não na
    // memória) e entram na mesma lista.
    if let Some(list) = value.get_mut("tools").and_then(Value::as_array_mut) {
        list.extend(crate::ui::tool_descriptors());
    }
    value
}

/// Os 8 primeiros caracteres do id, como aparecem para o usuário.
fn short_id(id: &str) -> &str {
    &id[..8.min(id.len())]
}

/// A decisão pendente cujo id começa com `id` (há de ser uma só).
fn pending_by_prefix(
    store: &MemoryStore,
    id: &str,
) -> Result<orchestrator_memory::store::PendingDecision, (i64, String)> {
    let id = id.trim();
    if id.len() < 4 {
        return Err((-32602, "id curto demais: use pelo menos 4 caracteres".to_string()));
    }
    let mut achadas: Vec<_> = store
        .list_pending_decisions(true)
        .map_err(|e| (-32000, e.to_string()))?
        .into_iter()
        .filter(|d| d.id.starts_with(id))
        .collect();
    match achadas.len() {
        0 => Err((-32602, format!("nenhuma decisão pendente com id {id}"))),
        1 => Ok(achadas.remove(0)),
        _ => Err((-32602, format!("mais de uma decisão começa com {id}: use o id inteiro"))),
    }
}

fn str_arg<'a>(args: &'a Value, key: &str) -> Result<&'a str, (i64, String)> {
    args.get(key)
        .and_then(Value::as_str)
        .ok_or((-32602, format!("argumento obrigatório ausente: {key}")))
}

fn tools_call(
    store: &MemoryStore,
    params: &Value,
    gate: Option<&McpGate>,
) -> Result<Value, (i64, String)> {
    let name = params.get("name").and_then(Value::as_str).unwrap_or("");
    let args = params.get("arguments").cloned().unwrap_or(json!({}));

    // Modo trava: a mesma decisão do hook, antes de executar. Bloqueio volta
    // como resultado de erro que o modelo lê (não como falha do protocolo),
    // e falha da própria trava fecha a porta.
    if let Some(g) = gate {
        if name != "permission_prompt" {
            let tool = gate::canonical_tool_name(name);
            let input = args.to_string();
            let project = project_of(&args);
            let call = gate::ToolCall {
                tool_name: &tool,
                tool_input: &input,
                session_id: &g.session_id,
                project: &project,
            };
            match gate::decide(store, &call, &g.options) {
                Ok(gate::Decision::Deny(reason)) => {
                    return Ok(json!({
                        "content": [{ "type": "text", "text": format!("Bloqueado pelo Orchestrator: {reason}") }],
                        "isError": true
                    }));
                }
                Ok(_) => {}
                Err(e) => return Err((-32000, format!("a trava do Orchestrator falhou: {e:#}"))),
            }
        }
    }

    // A tool de permissão devolve um JSON de veredito (contrato do Claude
    // Code) como texto — formato diferente das demais, então trata à parte.
    if name == "permission_prompt" {
        return Ok(permission_prompt(store, &args));
    }
    // Sandbox: age no container/navegador, sem passar pela fila da TUI.
    if crate::ui::handles(name) {
        let text = crate::ui::call_logged(store, &project_of(&args), name, &args)?;
        return Ok(json!({ "content": [{ "type": "text", "text": text }] }));
    }

    let text = match name {
        "retrieve_memory" => {
            let project = project_of(&args);
            let query = str_arg(&args, "query")?;
            let top_k = args
                .get("top_k")
                .and_then(Value::as_u64)
                .unwrap_or(5)
                .clamp(1, 20) as usize;
            let kind = kind_arg(&args)?;
            let (achados, semantica) = buscar_memoria(store, &project, query, top_k, kind)?;
            if achados.is_empty() {
                format!("Nenhuma memória relevante em \"{project}\" (nem global).")
            } else {
                let como = if semantica {
                    "busca semântica (ChromaDB + reranker)"
                } else {
                    "busca por palavras-chave (o serviço de memória não respondeu agora)"
                };
                let corpo = achados
                    .iter()
                    .map(|(m, score)| {
                        format!(
                            "[{} · {} · p{} · score {score:.3}] {}\n{}",
                            m.origin_label(),
                            m.kind,
                            m.priority,
                            m.title,
                            m.body
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n---\n");
                format!("{como}\n\n{corpo}")
            }
        }
        "store_memory" => {
            let project = project_of(&args);
            let kind = MemoryKind::from_str(str_arg(&args, "kind")?)
                .map_err(|e| (-32602i64, e.to_string()))?;
            let title = str_arg(&args, "title")?;
            let body = str_arg(&args, "body")?;
            let priority = args.get("priority").and_then(Value::as_i64).unwrap_or(0);
            // Quem grava por esta tool é sempre uma IA: fica no projeto, com o
            // nome dela, e nunca como regra do dono nem como global.
            let author = std::env::var("ORCHESTRATOR_AGENT").unwrap_or_else(|_| "IA".to_string());
            let mut nova = orchestrator_memory::store::NewMemory::agent(
                &project, &author, kind, title, body, priority,
            );
            if args.get("global").and_then(Value::as_bool).unwrap_or(false) {
                nova.scope = orchestrator_memory::Scope::Global;
            }
            let m = store.add(nova).map_err(|e| (-32000, e.to_string()))?;
            // Avisa o memoryd para indexar já; se ele não responder, a
            // indexação periódica pega depois (a memória já está no SQLite).
            let _ = orchestrator_memory::daemon::call(
                &orchestrator_memory::daemon::Request::Index {
                    ids: vec![m.id.clone()],
                },
                orchestrator_memory::daemon::HEALTH_TIMEOUT,
            );
            format!(
                "Memória gravada em \"{}\": {} ({}, autor {})",
                m.project, m.id, m.kind, m.author
            )
        }
        "ssh_exec" => ssh_exec(store, &project_of(&args), &args)?,
        "list_memories" => {
            let project = project_of(&args);
            let kind = kind_arg(&args)?;
            let list = store
                .list_visible(&project, kind)
                .map_err(|e| (-32000, e.to_string()))?;
            if list.is_empty() {
                "Nenhuma memória.".to_string()
            } else {
                list.iter()
                    .map(|m| {
                        format!(
                            "[{} · {} · p{}] {}\n{}",
                            m.origin_label(),
                            m.kind,
                            m.priority,
                            m.title,
                            m.body
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n---\n")
            }
        }
        "log_decision" => {
            let entry = store
                .log_decision(
                    str_arg(&args, "project")?,
                    str_arg(&args, "session_id")?,
                    str_arg(&args, "action")?,
                    str_arg(&args, "decision")?,
                    str_arg(&args, "reason")?,
                )
                .map_err(|e| (-32000, e.to_string()))?;
            format!("Decisão registrada: {}", entry.id)
        }
        "cli_start" => {
            let cli = str_arg(&args, "name")?;
            let command = args
                .get("command")
                .and_then(Value::as_str)
                .filter(|c| !c.trim().is_empty())
                .unwrap_or("claude");
            let cli_args: Vec<String> = args
                .get("args")
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(|v| v.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default();
            let cwd = args.get("cwd").and_then(Value::as_str).unwrap_or("");
            let payload = json!({ "command": command, "args": cli_args, "cwd": cwd }).to_string();
            enqueue_cli_and_wait(store, &project_of(&args), cli, "start", &payload)?
        }
        "cli_send" => {
            let cli = str_arg(&args, "name")?;
            let prompt = str_arg(&args, "prompt")?;
            enqueue_cli_and_wait(store, &project_of(&args), cli, "send", prompt)?
        }
        "cli_stop" => {
            let cli = str_arg(&args, "name")?;
            enqueue_cli_and_wait(store, &project_of(&args), cli, "stop", "")?
        }
        "cli_read" => {
            let cli = str_arg(&args, "name")?;
            let payload = json!({
                "search": args.get("search").and_then(Value::as_str).unwrap_or(""),
                "lines": args.get("lines").and_then(Value::as_u64).unwrap_or(40),
                "context": args.get("context").and_then(Value::as_u64).unwrap_or(3),
            })
            .to_string();
            enqueue_cli_and_wait(store, &project_of(&args), cli, "read", &payload)?
        }
        "cli_menu" => {
            let cli = str_arg(&args, "name")?;
            enqueue_cli_and_wait(store, &project_of(&args), cli, "menu", "")?
        }
        "cli_choose" => {
            let cli = str_arg(&args, "name")?;
            let payload = json!({
                "opcao": str_arg(&args, "opcao")?,
                "resposta": args.get("resposta").and_then(Value::as_str).unwrap_or(""),
            })
            .to_string();
            enqueue_cli_and_wait(store, &project_of(&args), cli, "choose", &payload)?
        }
        "cli_key" => {
            let cli = str_arg(&args, "name")?;
            let payload = json!({
                "tecla": str_arg(&args, "tecla")?,
                "vezes": args.get("vezes").and_then(Value::as_u64).unwrap_or(1).clamp(1, 20),
            })
            .to_string();
            enqueue_cli_and_wait(store, &project_of(&args), cli, "key", &payload)?
        }
        "cli_status" => {
            let project = project_of(&args);
            let lines = args
                .get("lines")
                .and_then(Value::as_u64)
                .unwrap_or(STATUS_LINES as u64)
                .clamp(1, 60) as usize;
            match args.get("name").and_then(Value::as_str) {
                Some(cli) => match store
                    .get_cli_state(&project, cli)
                    .map_err(|e| (-32000i64, e.to_string()))?
                {
                    Some((status, screen, updated_at)) => format!(
                        "CLI \"{cli}\": {status} (atualizado {updated_at})\n\
                         --- fim da tela ({lines} linhas; use cli_read para o histórico) ---\n{}",
                        tail_lines(&screen, lines)
                    ),
                    None => format!(
                        "Nenhuma CLI chamada \"{cli}\" no projeto \"{project}\". \
                         Use cli_start para abrir uma."
                    ),
                },
                None => {
                    let list = store
                        .list_cli_states(&project)
                        .map_err(|e| (-32000i64, e.to_string()))?;
                    if list.is_empty() {
                        format!(
                            "Nenhuma CLI aberta no projeto \"{project}\". \
                             Use cli_start para abrir uma."
                        )
                    } else {
                        list.iter()
                            .map(|(name, status, updated)| {
                                format!("- {name}: {status} (atualizado {updated})")
                            })
                            .collect::<Vec<_>>()
                            .join("\n")
                    }
                }
            }
        }
        "decision_resolve" => {
            let d = pending_by_prefix(store, str_arg(&args, "id")?)?;
            let approve = args
                .get("approve")
                .and_then(Value::as_bool)
                .ok_or((-32602, "argumento obrigatório ausente: approve".to_string()))?;
            let reason = str_arg(&args, "reason")?;
            if d.is_for_owner() || d.is_question() {
                return Err((
                    -32602,
                    "essa decisão é do dono: ele responde no app ou na TUI. Siga outra frente \
                     enquanto isso."
                        .to_string(),
                ));
            }
            store
                .resolve_decision(
                    &d.id,
                    approve,
                    &format!("{} {reason}", gate::RESOLVED_BY_ORCHESTRATOR),
                )
                .map_err(|e| (-32000, e.to_string()))?;
            let quem = if d.requester.is_empty() {
                "a CLI".to_string()
            } else {
                format!("a CLI \"{}\"", d.requester)
            };
            if approve {
                format!(
                    "Decisão {} aprovada. Avise {quem} com cli_send para tentar de novo: a mesma \
                     ação agora passa.",
                    short_id(&d.id)
                )
            } else {
                format!(
                    "Decisão {} negada. Avise {quem} com cli_send do motivo e do caminho a seguir.",
                    short_id(&d.id)
                )
            }
        }
        "decision_escalate" => {
            let d = pending_by_prefix(store, str_arg(&args, "id")?)?;
            let reason = str_arg(&args, "reason")?;
            if d.is_for_owner() {
                return Err((-32602, "essa decisão já está com o dono".to_string()));
            }
            let d = store
                .escalate_decision(&d.id, reason)
                .map_err(|e| (-32000, e.to_string()))?;
            if !cfg!(test) {
                orchestrator_notify::notify_pending_decision(&d.project, &d.summary);
            }
            format!(
                "Decisão {} passada ao dono. A resposta chega como mensagem de sistema; siga \
                 outra frente enquanto isso.",
                short_id(&d.id)
            )
        }
        "ask_owner" => {
            let question = str_arg(&args, "question")?;
            let options: Vec<String> = args
                .get("options")
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(Value::as_str)
                        .map(str::trim)
                        .filter(|o| !o.is_empty())
                        .map(str::to_string)
                        .collect()
                })
                .unwrap_or_default();
            let multiple = args.get("multiple").and_then(Value::as_bool).unwrap_or(false);
            let project = project_of(&args);
            let session = std::env::var("ORCHESTRATOR_SESSION")
                .ok()
                .filter(|s| !s.trim().is_empty())
                .unwrap_or_else(|| "chat".to_string());
            let requester = std::env::var("ORCHESTRATOR_AGENT")
                .unwrap_or_else(|_| gate::ORCHESTRATOR_AGENT.to_string());
            let q = store
                .ask_owner(&project, &session, question, &options, multiple, &requester)
                .map_err(|e| (-32000, e.to_string()))?;
            if !cfg!(test) {
                orchestrator_notify::notify_pending_decision(&project, question);
            }
            format!(
                "Pergunta registrada ({}). A resposta do dono chega como mensagem de sistema; \
                 siga outra frente enquanto isso.",
                short_id(&q.id)
            )
        }
        other => return Err((-32602, format!("tool desconhecida: {other}"))),
    };

    Ok(json!({ "content": [{ "type": "text", "text": text }] }))
}

/// Implementa a tool `permission_prompt` (alvo do `--permission-prompt-tool`).
///
/// Avalia a tool call contra as regras de segurança:
/// - `deny-regex` → nega na hora;
/// - caso contrário → enfileira uma decisão para o usuário e **espera**
///   (poll com timeout) a resolução na TUI/chat. É o modo síncrono, mais
///   seguro e menos autônomo: a vez do agente fica pausada até a decisão.
///
/// Sempre devolve o envelope MCP `{content:[{type:text, text:<json>}]}` com o
/// veredito no contrato do Claude Code (`allow`/`deny`) — nunca um erro
/// JSON-RPC, para o `claude` conseguir parsear a resposta.
fn permission_prompt(store: &MemoryStore, args: &Value) -> Value {
    let tool_name = args
        .get("tool_name")
        .or_else(|| args.get("toolName"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let input = args
        .get("input")
        .or_else(|| args.get("tool_input"))
        .cloned()
        .unwrap_or_else(|| json!({}));
    let input_str = input.to_string();
    // Projeto/sessão: arg tem precedência (testes), senão env do subprocesso.
    let project = args
        .get("project")
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| std::env::var("ORCHESTRATOR_PROJECT").unwrap_or_else(|_| "default".into()));
    let session_id = args
        .get("session_id")
        .or_else(|| args.get("tool_use_id"))
        .and_then(Value::as_str)
        .unwrap_or("mcp-permission")
        .to_string();
    let action = format!("{tool_name} {input_str}");

    let verdict = evaluate(store, &project, &tool_name, &input_str);
    let decision = match verdict {
        Verdict::Deny { rule_title } => {
            let reason = format!(
                "Bloqueado pela regra de segurança do Orchestrator: \"{rule_title}\"."
            );
            let _ = store.log_decision(&project, &session_id, &action, "blocked", &reason);
            deny_value(&reason)
        }
        // Allow (limpa) ou Ask (regra ask-regex): pergunta e espera.
        other => {
            let rule = match other {
                Verdict::Ask { rule_title, .. } => Some(rule_title),
                _ => None,
            };
            let summary = match &rule {
                Some(t) => format!("[{t}] {tool_name}: {input_str}"),
                None => format!("[permissão] {tool_name}: {input_str}"),
            };
            wait_for_decision(store, &project, &session_id, &summary, &action, &input)
        }
    };

    json!({ "content": [{ "type": "text", "text": decision.to_string() }] })
}

/// Enfileira (se preciso) e faz poll da decisão até resolver ou estourar o
/// timeout. Devolve o objeto de veredito `{behavior, ...}`.
fn wait_for_decision(
    store: &MemoryStore,
    project: &str,
    session_id: &str,
    summary: &str,
    action: &str,
    input: &Value,
) -> Value {
    // Estado atual: aproveita decisão já tomada e só enfileira na 1ª vez.
    match latest_status(store, project, session_id, summary).as_deref() {
        Some("approved") => {
            let _ = store.log_decision(project, session_id, action, "allowed", "Aprovado pelo usuário.");
            return allow_value(input);
        }
        Some("denied") => return deny_value("Negado pelo usuário no Orchestrator."),
        Some("pending") => {} // já na fila: segue direto pro poll
        _ => {
            let _ = store.enqueue_decision(project, session_id, summary);
            orchestrator_notify::notify_pending_decision(project, summary);
        }
    }

    let timeout = Duration::from_secs(
        std::env::var("ORCHESTRATOR_DECISION_TIMEOUT_SECS")
            .ok()
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(DEFAULT_DECISION_TIMEOUT_SECS),
    );
    let start = Instant::now();
    loop {
        std::thread::sleep(Duration::from_secs(1));
        match latest_status(store, project, session_id, summary).as_deref() {
            Some("approved") => {
                let _ =
                    store.log_decision(project, session_id, action, "allowed", "Aprovado pelo usuário.");
                return allow_value(input);
            }
            Some("denied") => return deny_value("Negado pelo usuário no Orchestrator."),
            _ => {}
        }
        if start.elapsed() >= timeout {
            return deny_value(
                "Tempo esgotado aguardando a decisão do usuário no Orchestrator (fail-closed).",
            );
        }
    }
}

/// Projeto alvo: argumento tem precedência (testes), senão a env var que a
/// TUI exporta ao subprocesso `claude`.
/// Tipo opcional de memória vindo dos argumentos da tool.
fn kind_arg(args: &Value) -> Result<Option<MemoryKind>, (i64, String)> {
    match args.get("kind").and_then(Value::as_str) {
        Some(k) => MemoryKind::from_str(k)
            .map(Some)
            .map_err(|e| (-32602i64, e.to_string())),
        None => Ok(None),
    }
}

/// Memórias achadas com o score, e se a busca foi semântica.
type Achados = (Vec<(orchestrator_memory::Memory, f32)>, bool);

/// Busca pelo memoryd (semântica); se ele não responder, léxico do SQLite.
///
/// O SQLite é quem manda: um id que o índice devolver e não existir mais é
/// descartado aqui.
fn buscar_memoria(
    store: &MemoryStore,
    project: &str,
    query: &str,
    top_k: usize,
    kind: Option<MemoryKind>,
) -> Result<Achados, (i64, String)> {
    use orchestrator_memory::daemon;
    let pedido = daemon::Request::Search {
        project: project.to_string(),
        query: query.to_string(),
        top_k,
        kind: kind.map(|k| k.as_str().to_string()),
    };
    // Nos testes, nunca o memoryd de verdade: ele responderia sobre o banco
    // do usuário (e, fora do ar, seria disparado por um teste).
    if !cfg!(test) {
        if let Ok(resposta) = daemon::call(&pedido, daemon::REQUEST_TIMEOUT) {
            let achados = resposta
                .hits
                .into_iter()
                .filter_map(|hit| store.get(&hit.id).ok().flatten().map(|m| (m, hit.score)))
                .collect();
            return Ok((achados, resposta.semantic));
        }
        let _ = daemon::spawn_detached();
    }
    let achados = store
        .search(project, query, top_k, kind)
        .map_err(|e| (-32000, e.to_string()))?
        .into_iter()
        .map(|r| (r.memory, r.score))
        .collect();
    Ok((achados, false))
}

/// `ssh_exec`: roda um comando num servidor cadastrado pelo dono para o
/// projeto (sem `host`, lista os cadastrados).
fn ssh_exec(store: &MemoryStore, project: &str, args: &Value) -> Result<String, (i64, String)> {
    use orchestrator_core::ssh;
    let hosts = store
        .ui_get(&ssh::state_key(project))
        .ok()
        .flatten()
        .map(|j| ssh::parse(&j))
        .unwrap_or_default();
    let nomes = || {
        hosts
            .iter()
            .map(|h| format!("- {} ({}@{}:{})", h.nome, h.usuario, h.host, h.porta))
            .collect::<Vec<_>>()
            .join("\n")
    };
    let Some(host) = args.get("host").and_then(Value::as_str).filter(|s| !s.trim().is_empty()) else {
        return Ok(if hosts.is_empty() {
            "nenhum servidor SSH cadastrado neste projeto — o dono cadastra em Conexões SSH no app".into()
        } else {
            format!("servidores cadastrados:\n{}", nomes())
        });
    };
    let Some(alvo) = hosts.iter().find(|h| h.nome.eq_ignore_ascii_case(host.trim())) else {
        return Err((-32602, format!("servidor \"{host}\" não cadastrado. Cadastrados:\n{}", nomes())));
    };
    let command = str_arg(args, "command")?;
    let cfg = ssh::write_config(project, &hosts).map_err(|e| (-32000, format!("config ssh: {e}")))?;
    let out = std::process::Command::new("timeout")
        .arg("120")
        .arg("ssh")
        .arg("-F")
        .arg(&cfg)
        .arg(ssh::alias(&alvo.nome))
        .arg("--")
        .arg(command)
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|e| (-32000, format!("não consegui rodar o ssh: {e}")))?;
    let mut texto = String::from_utf8_lossy(&out.stdout).to_string();
    let err = String::from_utf8_lossy(&out.stderr);
    if !err.trim().is_empty() {
        texto.push_str(&format!("\n[stderr]\n{err}"));
    }
    let linhas: Vec<&str> = texto.lines().collect();
    let corte = linhas.len().saturating_sub(80);
    let marca = match out.status.code() {
        Some(0) => "✔".to_string(),
        Some(124) => "✖ passou de 120s e foi cortado".to_string(),
        Some(c) => format!("✖ saiu com código {c}"),
        None => "✖ interrompido".to_string(),
    };
    Ok(format!("{}@{} $ {command}  {marca}\n{}", alvo.usuario, alvo.nome, linhas[corte..].join("\n")))
}

fn project_of(args: &Value) -> String {
    args.get("project")
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| {
            std::env::var("ORCHESTRATOR_PROJECT").unwrap_or_else(|_| "default".into())
        })
}

/// Últimas `n` linhas não vazias de um texto.
fn tail_lines(text: &str, n: usize) -> String {
    let kept: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    let start = kept.len().saturating_sub(n);
    let mut out = String::new();
    if start > 0 {
        out.push_str(&format!("(… {start} linhas antes — cli_read mostra)\n"));
    }
    out.push_str(&kept[start..].join("\n"));
    out
}

/// Enfileira um comando de CLI para a TUI e espera o ack (done/failed).
///
/// A TUI é o único processo que pode mexer nos PTYs, então toda tool `cli_*`
/// que age passa por esta fila no banco (mesma ideia da fila de decisões).
/// Timeout curto: sem TUI rodando, ninguém consome a fila.
fn enqueue_cli_and_wait(
    store: &MemoryStore,
    project: &str,
    cli_name: &str,
    kind: &str,
    payload: &str,
) -> Result<String, (i64, String)> {
    let cli_name = cli_name.trim();
    if cli_name.is_empty() {
        return Err((-32602, "o nome da CLI não pode ser vazio".into()));
    }
    let cmd = store
        .enqueue_cli_command(project, cli_name, kind, payload)
        .map_err(|e| (-32000i64, e.to_string()))?;
    let secs = cli_ack_timeout_secs();
    let timeout = Duration::from_secs(secs);
    let start = Instant::now();
    loop {
        std::thread::sleep(Duration::from_millis(300));
        match store.cli_command_status(&cmd.id) {
            Ok(Some((status, result))) => match status.as_str() {
                "done" => {
                    return Ok(result.unwrap_or_else(|| format!("{kind} ok: {cli_name}")))
                }
                "failed" => {
                    return Err((
                        -32000,
                        result.unwrap_or_else(|| format!("{kind} falhou: {cli_name}")),
                    ))
                }
                _ => {}
            },
            Ok(None) => return Err((-32000, "comando desapareceu da fila".into())),
            Err(e) => return Err((-32000, e.to_string())),
        }
        if start.elapsed() >= timeout {
            return Err((
                -32000,
                format!(
                    "ninguém executou o comando em {secs}s — a TUI do \
                     Orchestrator (`orchestrator tui`) precisa estar rodando \
                     para abrir/dirigir CLIs"
                ),
            ));
        }
    }
}

fn latest_status(
    store: &MemoryStore,
    project: &str,
    session_id: &str,
    summary: &str,
) -> Option<String> {
    store
        .latest_decision_status(project, session_id, summary)
        .ok()
        .flatten()
}

fn allow_value(input: &Value) -> Value {
    json!({ "behavior": "allow", "updatedInput": input })
}

fn deny_value(message: &str) -> Value {
    json!({ "behavior": "deny", "message": message })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> MemoryStore {
        MemoryStore::open_in_memory().unwrap()
    }

    #[test]
    fn initialize_and_tools_list() {
        let s = store();
        let resp = handle_line(
            &s,
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#,
        )
        .unwrap();
        let v: Value = serde_json::from_str(&resp).unwrap();
        assert_eq!(v["result"]["protocolVersion"], PROTOCOL_VERSION);

        let resp = handle_line(&s, r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#).unwrap();
        let v: Value = serde_json::from_str(&resp).unwrap();
        let names: Vec<&str> = v["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap())
            .collect();
        assert_eq!(
            names,
            [
                "retrieve_memory",
                "store_memory",
                "ssh_exec",
                "list_memories",
                "log_decision",
                "permission_prompt",
                "cli_start",
                "cli_send",
                "cli_status",
                "cli_read",
                "cli_menu",
                "cli_choose",
                "cli_key",
                "cli_stop",
                "decision_resolve",
                "decision_escalate",
                "ask_owner",
                "ui_open",
                "ui_snapshot",
                "ui_click",
                "ui_type",
                "ui_select",
                "ui_screenshot",
                "ui_exec",
                "ui_stop"
            ]
        );
    }

    #[test]
    fn notifications_get_no_response() {
        let s = store();
        assert!(handle_line(
            &s,
            r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#
        )
        .is_none());
        assert!(handle_line(&s, "not json").is_none());
    }

    #[test]
    fn store_then_retrieve_roundtrip() {
        let s = store();
        let resp = handle_line(
            &s,
            r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"store_memory","arguments":{"project":"p","kind":"security","title":"nunca rm -rf","body":"proibido rm -rf em qualquer diretório"}}}"#,
        )
        .unwrap();
        let v: Value = serde_json::from_str(&resp).unwrap();
        assert!(v["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .starts_with("Memória gravada"));

        let resp = handle_line(
            &s,
            r#"{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"retrieve_memory","arguments":{"project":"p","query":"posso usar rm -rf?"}}}"#,
        )
        .unwrap();
        let v: Value = serde_json::from_str(&resp).unwrap();
        assert!(v["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("nunca rm -rf"));
    }

    #[test]
    fn unknown_method_is_error() {
        let s = store();
        let resp = handle_line(&s, r#"{"jsonrpc":"2.0","id":9,"method":"nope"}"#).unwrap();
        let v: Value = serde_json::from_str(&resp).unwrap();
        assert_eq!(v["error"]["code"], -32601);
    }

    /// Extrai o objeto de veredito (`{behavior, ...}`) de dentro do envelope
    /// MCP `{content:[{type:text, text:<json>}]}` devolvido pela tool.
    #[test]
    fn orchestrator_resolves_its_decisions_and_escalates_or_asks_the_owner() {
        use orchestrator_memory::store::REVIEWER_ORCHESTRATOR;
        let s = store();
        let chamar = |nome: &str, args: Value| tools_call(&s, &json!({ "name": nome, "arguments": args }), None);
        let texto = |v: &Value| v["content"][0]["text"].as_str().unwrap_or("").to_string();

        let da_cli = s
            .enqueue_decision_for("p", "cli-1", "[migrações] Bash: sqlx migrate run", REVIEWER_ORCHESTRATOR, "backend")
            .unwrap();
        let do_dono = s.enqueue_decision("p", "cli-2", "[deploy] Bash: npm publish").unwrap();

        // A do dono não é do orquestrador.
        let erro = chamar("decision_resolve", json!({ "id": &do_dono.id[..8], "approve": true, "reason": "ok" }))
            .unwrap_err();
        assert!(erro.1.contains("dono"), "{}", erro.1);

        let r = chamar("decision_resolve", json!({ "id": &da_cli.id[..8], "approve": true, "reason": "a migração foi pedida" }))
            .unwrap();
        assert!(texto(&r).contains("backend"), "{r}");
        let depois = s.get_decision(&da_cli.id).unwrap().unwrap();
        assert_eq!(depois.status, "approved");
        assert!(depois.resolution.unwrap().starts_with(gate::RESOLVED_BY_ORCHESTRATOR));

        let critica = s
            .enqueue_decision_for("p", "cli-1", "[git] Bash: git push --force", REVIEWER_ORCHESTRATOR, "backend")
            .unwrap();
        chamar("decision_escalate", json!({ "id": &critica.id, "reason": "reescreve o main, não foi pedido" })).unwrap();
        assert!(s.get_decision(&critica.id).unwrap().unwrap().is_for_owner());

        let r = chamar(
            "ask_owner",
            json!({ "project": "p", "question": "Qual banco usar?", "options": ["Postgres", " ", "SQLite"], "multiple": false }),
        )
        .unwrap();
        assert!(texto(&r).contains("Pergunta registrada"), "{r}");
        let pergunta = s
            .list_pending_decisions(true)
            .unwrap()
            .into_iter()
            .find(|d| d.is_question())
            .unwrap();
        assert_eq!(pergunta.options, ["Postgres", "SQLite"]);
    }

    #[test]
    fn gate_in_mcp_blocks_changes_until_the_session_consults() {
        let s = store();
        let g = McpGate {
            session_id: "chat-1".into(),
            options: gate::Options { autonomous: true, notify: false, requester: None },
        };
        let chamar = |nome: &str| -> Value {
            let req = json!({"jsonrpc":"2.0","id":1,"method":"tools/call",
                "params":{"name":nome,"arguments":{"project":"p","name":"x"}}});
            serde_json::from_str(&handle_line_with(&s, &req.to_string(), Some(&g)).unwrap()).unwrap()
        };
        // Tool que muda algo, antes da consulta: bloqueio que o modelo lê.
        let r = chamar("cli_inexistente");
        assert_eq!(r["result"]["isError"], true, "{r}");
        assert!(r["result"]["content"][0]["text"].as_str().unwrap().contains("retrieve_memory"));
        // Consultou (a consulta vale para a sessão do chat inteira): a trava
        // deixa passar e a chamada segue para o despacho normal.
        s.record_consult("chat-1", "p").unwrap();
        let r = chamar("cli_inexistente");
        assert!(r["error"]["message"].as_str().unwrap().contains("tool desconhecida"), "{r}");
        // Sem modo trava (quem tem hook), o servidor não decide nada.
        let req = json!({"jsonrpc":"2.0","id":2,"method":"tools/call",
            "params":{"name":"cli_inexistente","arguments":{"project":"outro"}}});
        let r: Value = serde_json::from_str(&handle_line_with(&s, &req.to_string(), None).unwrap()).unwrap();
        assert!(r["error"]["message"].as_str().unwrap().contains("tool desconhecida"), "{r}");
    }

    fn verdict_of(envelope: &Value) -> Value {
        let text = envelope["content"][0]["text"].as_str().expect("text no envelope");
        serde_json::from_str(text).expect("veredito é JSON")
    }

    #[test]
    fn permission_prompt_denies_on_deny_regex() {
        let s = store();
        s.add_memory(
            "p",
            MemoryKind::Security,
            "nunca rm -rf",
            "Comandos destrutivos são proibidos.\ndeny-regex: rm\\s+-rf",
            10,
        )
        .unwrap();

        let args = json!({
            "tool_name": "Bash",
            "input": { "command": "rm -rf /tmp/x" },
            "project": "p",
            "session_id": "sess-deny"
        });
        let v = verdict_of(&permission_prompt(&s, &args));
        assert_eq!(v["behavior"], "deny");
        assert!(
            v["message"].as_str().unwrap().contains("nunca rm -rf"),
            "mensagem deveria citar a regra; foi: {v}"
        );

        // Negação por regra de segurança é auditada como "blocked".
        let decisions = s.list_decisions("p").unwrap();
        assert!(decisions.iter().any(|d| d.decision == "blocked"));
    }

    #[test]
    fn permission_prompt_allows_when_preapproved() {
        let s = store();
        // Sem regra casando: tool limpa. Pré-aprovamos a decisão de mesmo
        // resumo para que o poll retorne "allow" imediatamente (sem esperar).
        let summary = r#"[permissão] Bash: {"command":"ls"}"#;
        let pending = s.enqueue_decision("p", "sess-allow", summary).unwrap();
        s.resolve_decision(&pending.id, true, "ok").unwrap();

        let args = json!({
            "tool_name": "Bash",
            "input": { "command": "ls" },
            "project": "p",
            "session_id": "sess-allow"
        });
        let v = verdict_of(&permission_prompt(&s, &args));
        assert_eq!(v["behavior"], "allow");
        assert_eq!(v["updatedInput"], json!({ "command": "ls" }));
    }

    #[test]
    fn cli_start_enqueues_and_returns_tui_result() {
        let s = store();
        // A TUI (outro processo) consome a fila; aqui simulamos numa thread.
        std::thread::scope(|scope| {
            scope.spawn(|| {
                for _ in 0..100 {
                    if let Ok(cmds) = s.pending_cli_commands("proj") {
                        if let Some(c) = cmds.first() {
                            assert_eq!(c.kind, "start");
                            assert_eq!(c.cli_name, "frontend");
                            let payload: Value = serde_json::from_str(&c.payload).unwrap();
                            assert_eq!(payload["command"], "claude");
                            let _ = s.finish_cli_command(
                                &c.id,
                                true,
                                "CLI \"frontend\" aberta na workspace 1",
                            );
                            return;
                        }
                    }
                    std::thread::sleep(Duration::from_millis(20));
                }
                panic!("comando nunca apareceu na fila");
            });
            let out = tools_call(
                &s,
                &json!({
                    "name": "cli_start",
                    "arguments": { "name": "frontend", "project": "proj" }
                }),
                None,
            )
            .unwrap();
            let text = out["content"][0]["text"].as_str().unwrap();
            assert!(text.contains("frontend"), "resposta inesperada: {text}");
        });
    }

    #[test]
    fn cli_send_reports_failure_from_tui() {
        let s = store();
        std::thread::scope(|scope| {
            scope.spawn(|| {
                for _ in 0..100 {
                    if let Ok(cmds) = s.pending_cli_commands("proj") {
                        if let Some(c) = cmds.first() {
                            assert_eq!(c.kind, "send");
                            assert_eq!(c.payload, "faça o README");
                            let _ = s.finish_cli_command(
                                &c.id,
                                false,
                                "nenhuma CLI chamada \"frontend\"",
                            );
                            return;
                        }
                    }
                    std::thread::sleep(Duration::from_millis(20));
                }
                panic!("comando nunca apareceu na fila");
            });
            let err = tools_call(
                &s,
                &json!({
                    "name": "cli_send",
                    "arguments": {
                        "name": "frontend",
                        "prompt": "faça o README",
                        "project": "proj"
                    }
                }),
                None,
            )
            .unwrap_err();
            assert!(err.1.contains("nenhuma CLI"), "erro inesperado: {}", err.1);
        });
    }

    #[test]
    fn cli_status_reads_published_state() {
        let s = store();
        // Sem CLI: instrui a abrir uma.
        let out = tools_call(
            &s,
            &json!({ "name": "cli_status", "arguments": { "project": "proj" } }),
                None,
        )
        .unwrap();
        assert!(out["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("cli_start"));

        s.upsert_cli_state("proj", "frontend", "idle", "✔ README criado")
            .unwrap();
        let out = tools_call(
            &s,
            &json!({
                "name": "cli_status",
                "arguments": { "name": "frontend", "project": "proj" }
            }),
                None,
        )
        .unwrap();
        let text = out["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("idle"));
        assert!(text.contains("README criado"));

        // Listagem inclui a CLI publicada.
        let out = tools_call(
            &s,
            &json!({ "name": "cli_status", "arguments": { "project": "proj" } }),
                None,
        )
        .unwrap();
        assert!(out["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("frontend: idle"));
    }

    #[test]
    fn cli_command_times_out_without_a_tui() {
        // Sem executor, a tool falha com instrução clara. Timeout encurtado
        // por env var para a suíte não esperar os 15s do default.
        std::env::set_var("ORCHESTRATOR_CLI_ACK_TIMEOUT_SECS", "1");
        let s = store();
        let started = Instant::now();
        let err = tools_call(
            &s,
            &json!({
                "name": "cli_stop",
                "arguments": { "name": "fantasma", "project": "proj" }
            }),
                None,
        )
        .unwrap_err();
        assert!(err.1.contains("orchestrator tui"), "erro: {}", err.1);
        assert!(started.elapsed() >= Duration::from_secs(1));
        std::env::remove_var("ORCHESTRATOR_CLI_ACK_TIMEOUT_SECS");
    }

    #[test]
    fn cli_name_cannot_be_blank() {
        let s = store();
        let err = tools_call(
            &s,
            &json!({
                "name": "cli_send",
                "arguments": { "name": "   ", "prompt": "oi", "project": "proj" }
            }),
                None,
        )
        .unwrap_err();
        assert!(err.1.contains("vazio"));
    }

    #[test]
    fn tail_lines_keeps_the_end_and_says_what_was_cut() {
        let text = "linha1\n\nlinha2\nlinha3";
        assert_eq!(tail_lines(text, 10), "linha1\nlinha2\nlinha3");
        let cut = tail_lines(text, 1);
        assert!(cut.starts_with("(… 2 linhas antes"), "{cut}");
        assert!(cut.ends_with("linha3"));
        assert!(cut.contains("cli_read"), "precisa ensinar como ver o resto");
    }

    #[test]
    fn cli_status_is_short_by_default() {
        let s = store();
        let tela: String = (0..200)
            .map(|i| format!("linha {i}\n"))
            .collect::<Vec<_>>()
            .join("");
        s.upsert_cli_state("proj", "frontend", "idle", &tela).unwrap();
        let out = tools_call(
            &s,
            &json!({
                "name": "cli_status",
                "arguments": { "name": "frontend", "project": "proj" }
            }),
                None,
        )
        .unwrap();
        let text = out["content"][0]["text"].as_str().unwrap();
        // O que estourava o contexto era despejar a tela toda em toda consulta.
        let linhas = text.lines().filter(|l| l.starts_with("linha ")).count();
        assert_eq!(linhas, STATUS_LINES);
        assert!(text.contains("linha 199"), "deve trazer o FIM da tela");
        assert!(text.contains("cli_read"));

        // E dá para pedir mais quando realmente precisa.
        let out = tools_call(
            &s,
            &json!({
                "name": "cli_status",
                "arguments": { "name": "frontend", "project": "proj", "lines": 40 }
            }),
                None,
        )
        .unwrap();
        let text = out["content"][0]["text"].as_str().unwrap();
        assert_eq!(text.lines().filter(|l| l.starts_with("linha ")).count(), 40);
    }

    #[test]
    fn cli_read_goes_through_the_queue_with_search_options() {
        let s = store();
        std::thread::scope(|scope| {
            scope.spawn(|| {
                for _ in 0..100 {
                    if let Ok(cmds) = s.pending_cli_commands("proj") {
                        if let Some(c) = cmds.first() {
                            assert_eq!(c.kind, "read");
                            let payload: Value = serde_json::from_str(&c.payload).unwrap();
                            assert_eq!(payload["search"], "error");
                            assert_eq!(payload["context"], 3);
                            let _ = s.finish_cli_command(&c.id, true, "error: falhou aqui");
                            return;
                        }
                    }
                    std::thread::sleep(Duration::from_millis(20));
                }
                panic!("comando nunca apareceu na fila");
            });
            let out = tools_call(
                &s,
                &json!({
                    "name": "cli_read",
                    "arguments": { "name": "frontend", "search": "error", "project": "proj" }
                }),
                None,
            )
            .unwrap();
            assert!(out["content"][0]["text"]
                .as_str()
                .unwrap()
                .contains("falhou aqui"));
        });
    }
}
