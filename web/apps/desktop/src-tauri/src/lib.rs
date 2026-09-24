//! App desktop do Orchestrator.
//!
//! A janela desenha o MESMO [`Engine`] da TUI: um laço em segundo plano faz o
//! que o laço da TUI faz (drenar chat, CLIs, fila do MCP, decisões) e manda
//! uma foto do estado para a interface quando algo muda. Os comandos `/`
//! passam por [`Engine::run_command`], e a saída de cada CLI chega ao xterm.js
//! byte a byte, por canal.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::Serialize;
use tauri::ipc::{Channel, InvokeResponseBody};
use tauri::{AppHandle, Emitter, Manager, State};

use orchestrator_core::{config, Config};
use orchestrator_engine::palette::{self, Readiness};
use orchestrator_engine::workbench::{CommandOutcome, Engine, EngineEvent, Pane, RELOAD_EVERY};
use orchestrator_memory::store::MemoryStore;

mod remoto;

/// O núcleo, compartilhado entre os comandos, o laço e o servidor remoto.
/// `Arc` para o servidor web (`remoto`) segurar um clone do MESMO Engine.
struct Nucleo(Arc<Mutex<Engine>>);

const TICK: Duration = Duration::from_millis(100);

// ------------------------------------------------------------------ fotos

#[derive(Serialize, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
struct Linha {
    quem: String,
    texto: String,
}

#[derive(Serialize, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
struct Card {
    indice: usize,
    nome: String,
    /// `cli` (aberta pelo orquestrador ou por `/cli`), `terminal` (manual) ou `agente`.
    tipo: &'static str,
    /// `iniciando`, `trabalhando`, `ociosa`, `encerrada`.
    estado: &'static str,
    /// Agente: a tarefa atual e as opções (modelo, esforço…).
    detalhe: String,
    /// Agente: o fim da saída (terminais chegam pelo canal de bytes).
    saida: String,
    autopilot: String,
    consumo: String,
}

#[derive(Serialize, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
struct Workspace {
    numero: usize,
    pasta: String,
    pasta_propria: bool,
    focado: usize,
    cards: Vec<Card>,
}

#[derive(Serialize, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
struct Decisao {
    id: String,
    projeto: String,
    resumo: String,
    criada: String,
    /// `dono` ou `orquestrador` (modo autônomo).
    revisor: &'static str,
    /// A CLI que pediu, quando se sabe.
    pedido_por: String,
    /// `acao` (aprovar/negar) ou `pergunta`.
    tipo: &'static str,
    opcoes: Vec<String>,
    multipla: bool,
    /// Por que o orquestrador passou ao dono.
    nota: String,
}

#[derive(Serialize, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
struct TesteModelo {
    /// `"<provedor>/<modelo>"`.
    chave: String,
    ok: bool,
    /// A resposta (sucesso) ou o motivo (falha), curto.
    texto: String,
}

#[derive(Serialize, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
struct Foto {
    projetos: Vec<String>,
    projeto: String,
    workspace: usize,
    workspaces: Vec<Workspace>,
    chat: Vec<Linha>,
    chat_ocupado: bool,
    chat_parcial: String,
    chat_pensando: String,
    fila: Option<String>,
    provedor: String,
    modelo: String,
    postura: String,
    status: String,
    avisos: Vec<String>,
    decisoes: Vec<Decisao>,
    consumo: String,
    /// As opções do seletor de modelo do provedor ativo — a primeira é
    /// sempre "padrão do provedor"; o resto vem da API dele quando
    /// `sincronizar_modelos` já trouxe a lista ao vivo.
    modelos_opcoes: Vec<String>,
    modelos_carregando: bool,
    teste_modelo: Option<TesteModelo>,
}

fn consumo(entrada: u64, saida: u64, custo: Option<f64>) -> String {
    if entrada == 0 && saida == 0 && custo.is_none() {
        return String::new();
    }
    let mut s = format!("{entrada} → {saida} tok");
    if let Some(c) = custo {
        s.push_str(&format!(" · US$ {c:.4}"));
    }
    s
}

fn foto(e: &Engine) -> Foto {
    let workspaces = e
        .workspaces
        .iter()
        .enumerate()
        .map(|(i, ws)| Workspace {
            numero: i + 1,
            pasta: if i == e.ws_idx {
                e.workspace_dir().display().to_string()
            } else {
                ws.dir
                    .clone()
                    .unwrap_or_else(|| e.project_path())
                    .display()
                    .to_string()
            },
            pasta_propria: ws.dir.is_some(),
            focado: ws.focused,
            cards: ws
                .panes
                .iter()
                .enumerate()
                .map(|(indice, pane)| match pane {
                    Pane::Term(t) => Card {
                        indice,
                        nome: t.name.clone(),
                        tipo: if t.managed { "cli" } else { "terminal" },
                        estado: if t.is_exited() {
                            "encerrada"
                        } else {
                            match t.state().label() {
                                "working" => "trabalhando",
                                "idle" => "ociosa",
                                "exited" => "encerrada",
                                _ => "iniciando",
                            }
                        },
                        detalhe: t.last_prompt.clone(),
                        saida: String::new(),
                        autopilot: String::new(),
                        consumo: String::new(),
                    },
                    Pane::Agent(a) => {
                        let saida: String = {
                            let linhas: Vec<&str> = a.output.lines().collect();
                            linhas[linhas.len().saturating_sub(200)..].join("\n")
                        };
                        Card {
                            indice,
                            nome: a.name.clone(),
                            tipo: "agente",
                            estado: if a.busy {
                                "trabalhando"
                            } else if a.done {
                                "ociosa"
                            } else {
                                "iniciando"
                            },
                            detalhe: a.task.clone(),
                            saida,
                            autopilot: if a.goal_done {
                                "meta concluída".into()
                            } else if a.auto {
                                format!("autopilot {}/{}", a.iterations, a.max_iterations)
                            } else {
                                String::new()
                            },
                            consumo: consumo(a.tokens_in, a.tokens_out, a.cost),
                        }
                    }
                })
                .collect(),
        })
        .collect();
    let chat = e
        .chat
        .transcript
        .iter()
        .map(|l| Linha {
            quem: l.who.clone(),
            texto: l.text.clone(),
        })
        .collect();
    Foto {
        projetos: e.projects.clone(),
        projeto: e.project().to_string(),
        workspace: e.ws_idx,
        workspaces,
        chat,
        chat_ocupado: e.chat.busy,
        chat_parcial: e.chat.partial().to_string(),
        chat_pensando: e.chat.thinking().to_string(),
        fila: e.chat.queued.clone(),
        provedor: e.chat.provider.name.clone(),
        modelo: e.chat_model.clone(),
        postura: e.posture.label().to_string(),
        status: e.status.clone(),
        avisos: e.notices.clone(),
        decisoes: e
            .pending
            .iter()
            .map(|p| Decisao {
                id: p.id.clone(),
                projeto: p.project.clone(),
                resumo: p.summary.clone(),
                criada: p.created_at.clone(),
                revisor: if p.is_for_owner() { "dono" } else { "orquestrador" },
                pedido_por: p.requester.clone(),
                tipo: if p.is_question() { "pergunta" } else { "acao" },
                opcoes: p.options.clone(),
                multipla: p.multiple,
                nota: p.note.clone().unwrap_or_default(),
            })
            .collect(),
        consumo: consumo(e.chat.tokens_in, e.chat.tokens_out, e.chat.cost),
        modelos_opcoes: e.model_options(),
        modelos_carregando: e.models_loading(),
        teste_modelo: e.last_model_test.as_ref().map(|(chave, r)| match r {
            Ok(texto) => TesteModelo { chave: chave.clone(), ok: true, texto: texto.clone() },
            Err(texto) => TesteModelo { chave: chave.clone(), ok: false, texto: texto.clone() },
        }),
    }
}

fn pedido(evento: EngineEvent) -> &'static str {
    match evento {
        EngineEvent::FocusGrid => "focar-grade",
        EngineEvent::FocusChat => "focar-chat",
        EngineEvent::ChooseModel => "escolher-modelo",
        EngineEvent::ChooseProvider => "escolher-provedor",
        EngineEvent::ShowHelp => "abrir-manual",
    }
}

fn travar<'a>(nucleo: &'a State<'_, Nucleo>) -> Result<std::sync::MutexGuard<'a, Engine>, String> {
    nucleo.0.lock().map_err(|_| "o núcleo travou (mutex envenenado)".to_string())
}

fn avisar_pedidos(app: &AppHandle, eventos: Vec<EngineEvent>) {
    for evento in eventos {
        let _ = app.emit("pedido", pedido(evento));
    }
}

// ------------------------------------------------------------------ comandos

#[tauri::command]
fn estado(nucleo: State<'_, Nucleo>) -> Result<Foto, String> {
    let e = travar(&nucleo)?;
    Ok(foto(&e))
}

/// O que se digita no chat: comando `/` pelo mesmo despacho da TUI, ou
/// mensagem para o orquestrador.
#[tauri::command]
fn enviar(app: AppHandle, nucleo: State<'_, Nucleo>, texto: String) -> Result<(), String> {
    let eventos = {
        let mut e = travar(&nucleo)?;
        if e.run_command(&texto) == CommandOutcome::NotCommand {
            e.chat.input = texto;
            e.send_chat(None);
        }
        e.take_events()
    };
    avisar_pedidos(&app, eventos);
    Ok(())
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ItemPaleta {
    nome: &'static str,
    uso: &'static str,
    sobre: &'static str,
    /// `pronto`, `argumento` ou `indisponivel`.
    estado: &'static str,
    motivo: String,
}

/// Os comandos que casam com o que está digitado, com o estado de cada um.
#[tauri::command]
fn paleta(nucleo: State<'_, Nucleo>, texto: String) -> Result<Vec<ItemPaleta>, String> {
    let e = travar(&nucleo)?;
    let entrada = if texto.starts_with('/') { texto } else { format!("/{texto}") };
    Ok(palette::filter(&entrada, &e.palette_context())
        .into_iter()
        .map(|item| {
            let (estado, motivo) = match &item.readiness {
                Readiness::Ready => ("pronto", String::new()),
                Readiness::NeedsArgument(m) => ("argumento", m.clone()),
                Readiness::Unavailable(m) => ("indisponivel", m.clone()),
            };
            ItemPaleta {
                nome: item.command.name,
                uso: item.command.usage,
                sobre: item.command.about,
                estado,
                motivo,
            }
        })
        .collect())
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ItemProvedor {
    nome: String,
    ferramenta: &'static str,
    modelo: String,
    /// `pronto`, `login`, `instalar` ou `chave`.
    estado: &'static str,
    /// O que falta, quando não está pronto.
    dica: String,
    atual: bool,
}

/// A lista do `/provedor`, com o estado de cada um agora.
#[tauri::command]
fn provedores(nucleo: State<'_, Nucleo>) -> Result<Vec<ItemProvedor>, String> {
    let e = travar(&nucleo)?;
    let atual = e.chat.provider.name.clone();
    Ok(e.provider_list()
        .into_iter()
        .map(|(p, a)| ItemProvedor {
            atual: p.name == atual,
            ferramenta: p.kind.tool_label(),
            modelo: p.model.clone(),
            estado: a.state.code(),
            dica: a.hint,
            nome: p.name,
        })
        .collect())
}

/// Escolhe quem responde no chat. Devolve o que aconteceu (trocou, abriu o
/// card de login, ou o que falta).
#[tauri::command]
fn escolher_provedor(app: AppHandle, nucleo: State<'_, Nucleo>, nome: String) -> Result<String, String> {
    let (status, eventos) = {
        let mut e = travar(&nucleo)?;
        let Some(i) = e.providers.iter().position(|p| p.name == nome) else {
            return Err(format!("não existe provedor \"{nome}\""));
        };
        e.choose_provider(i);
        (e.status.clone(), e.take_events())
    };
    avisar_pedidos(&app, eventos);
    Ok(status)
}

/// Pede à API do provedor ativo a lista de modelos dela (o laço em segundo
/// plano traz o resultado no próximo `estado`).
#[tauri::command]
fn sincronizar_modelos(nucleo: State<'_, Nucleo>) -> Result<(), String> {
    travar(&nucleo)?.refresh_models();
    Ok(())
}

/// Manda "." ao modelo e mostra se ele processa e responde (resultado
/// também chega pelo `estado`, em `testeModelo`).
#[tauri::command]
fn testar_modelo(nucleo: State<'_, Nucleo>, modelo: String) -> Result<(), String> {
    travar(&nucleo)?.test_model(&modelo);
    Ok(())
}

/// A tela virtual (viva) de uma sandbox aberta pelo orquestrador, como
/// `data:` URL — pronta para um `<img src>`. `None` sem sandbox aberta com
/// esse nome, ou sem foto ainda (primeiro segundo depois do `ui_open`).
#[tauri::command]
fn tela_viva(nucleo: State<'_, Nucleo>, nome: String) -> Result<Option<String>, String> {
    let (caminho, em) = {
        let e = travar(&nucleo)?;
        match e.sandbox_live(&nome) {
            Some(v) => v,
            None => return Ok(None),
        }
    };
    let bytes = match std::fs::read(&caminho) {
        Ok(b) => b,
        // A thread de captura pode estar no meio de um rename() atômico —
        // a próxima volta do app (1.5s) tenta de novo.
        Err(_) => return Ok(None),
    };
    let _ = em;
    Ok(Some(format!("data:image/png;base64,{}", base64_encode(&bytes))))
}

/// Codifica base64 sem puxar dependência nova — o mesmo espírito de
/// `orchestrator_sandbox::session::decode_base64`, só que no sentido
/// inverso.
fn base64_encode(bytes: &[u8]) -> String {
    const TABELA: &[u8; 64] =
        b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b0 = chunk[0];
        let b1 = *chunk.get(1).unwrap_or(&0);
        let b2 = *chunk.get(2).unwrap_or(&0);
        out.push(TABELA[(b0 >> 2) as usize] as char);
        out.push(TABELA[(((b0 & 0x03) << 4) | (b1 >> 4)) as usize] as char);
        out.push(if chunk.len() > 1 {
            TABELA[(((b1 & 0x0f) << 2) | (b2 >> 6)) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            TABELA[(b2 & 0x3f) as usize] as char
        } else {
            '='
        });
    }
    out
}

// -------------------------------------------------------------------- IDE

/// Pastas que nunca valem a pena mostrar na árvore — build cheio de gente
/// gerada e nunca é o que o dono quer editar, só deixa a árvore lenta e
/// poluída.
const IDE_IGNORAR: &[&str] = &[
    "node_modules", "target", "dist", ".git", "__pycache__", ".venv", "venv",
];

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct EntradaArquivo {
    nome: String,
    /// Caminho relativo à raiz do projeto — é o que volta em `ide_ler`/`ide_salvar`.
    caminho: String,
    pasta: bool,
}

/// A raiz do projeto ativo, canônica — toda operação do IDE é confinada a
/// ela (nunca lê/escreve fora do projeto que o dono está vendo).
/// A raiz da IDE: a pasta da workspace ativa (a própria, se tiver; senão a
/// do projeto) — a mesma onde as CLIs desta workspace abrem.
fn raiz_projeto(nucleo: &State<'_, Nucleo>) -> Result<std::path::PathBuf, String> {
    let (projeto, caminho) = {
        let e = travar(nucleo)?;
        (e.project().to_string(), e.workspace_dir())
    };
    caminho.canonicalize().map_err(|_| {
        format!(
            "a pasta de \"{projeto}\" não existe mais ({}) — clique com o botão direito no projeto ou na workspace para escolher outra",
            caminho.display()
        )
    })
}

#[tauri::command]
fn alterar_pasta_projeto(nucleo: State<'_, Nucleo>, nome: String, pasta: String) -> Result<String, String> {
    let mut e = travar(&nucleo)?;
    e.set_project_path(&nome, &pasta);
    Ok(e.status.clone())
}

/// `pasta` = "-" volta a workspace para a pasta do projeto.
#[tauri::command]
fn alterar_pasta_workspace(nucleo: State<'_, Nucleo>, indice: usize, pasta: String) -> Result<String, String> {
    let mut e = travar(&nucleo)?;
    e.set_workspace_dir_at(indice, &pasta);
    Ok(e.status.clone())
}

#[tauri::command]
fn abrir_terminal(nucleo: State<'_, Nucleo>) -> Result<String, String> {
    let mut e = travar(&nucleo)?;
    e.open_shell();
    Ok(e.status.clone())
}

#[tauri::command]
fn abrir_ssh(nucleo: State<'_, Nucleo>, nome: String) -> Result<String, String> {
    let mut e = travar(&nucleo)?;
    e.open_ssh(&nome);
    Ok(e.status.clone())
}

#[tauri::command]
fn ssh_listar(nucleo: State<'_, Nucleo>) -> Result<Vec<orchestrator_core::ssh::SshHost>, String> {
    Ok(travar(&nucleo)?.ssh_hosts())
}

#[tauri::command]
fn ssh_salvar(nucleo: State<'_, Nucleo>, hosts: Vec<orchestrator_core::ssh::SshHost>) -> Result<String, String> {
    let mut e = travar(&nucleo)?;
    e.set_ssh_hosts(hosts);
    Ok(e.status.clone())
}

// ------------------------------------------------------ sandbox do dono

/// Um `orchestrator-mcp` filho, falando JSON-RPC por stdio — é por ele que o
/// DONO abre sandbox: a mesma porta que as IAs usam, então isolamento por
/// projeto, captura da tela virtual e readoção do container são os mesmos.
/// Fica vivo enquanto o app estiver aberto (a captura mora nele).
struct McpFilho {
    _child: std::process::Child,
    stdin: std::process::ChildStdin,
    stdout: std::io::BufReader<std::process::ChildStdout>,
    id: u64,
}

impl McpFilho {
    fn novo(env: &[(String, String)]) -> Result<Self, String> {
        let bin = std::env::current_exe()
            .map_err(|e| e.to_string())?
            .with_file_name("orchestrator-mcp");
        let mut child = std::process::Command::new(&bin)
            .envs(env.iter().map(|(k, v)| (k, v)))
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn()
            .map_err(|e| format!("não consegui abrir {}: {e}", bin.display()))?;
        let stdin = child.stdin.take().ok_or("sem stdin")?;
        let stdout = std::io::BufReader::new(child.stdout.take().ok_or("sem stdout")?);
        Ok(Self { _child: child, stdin, stdout, id: 0 })
    }

    fn chamar(&mut self, tool: &str, args: serde_json::Value) -> Result<String, String> {
        use std::io::{BufRead, Write};
        self.id += 1;
        let req = serde_json::json!({
            "jsonrpc": "2.0", "id": self.id, "method": "tools/call",
            "params": { "name": tool, "arguments": args }
        });
        writeln!(self.stdin, "{req}").map_err(|e| e.to_string())?;
        self.stdin.flush().map_err(|e| e.to_string())?;
        let mut linha = String::new();
        loop {
            linha.clear();
            if self.stdout.read_line(&mut linha).map_err(|e| e.to_string())? == 0 {
                return Err("o servidor da sandbox fechou".into());
            }
            let v: serde_json::Value = serde_json::from_str(&linha).unwrap_or_default();
            if v["id"] != serde_json::json!(self.id) {
                continue;
            }
            if let Some(err) = v.get("error") {
                return Err(err["message"].as_str().unwrap_or("erro").to_string());
            }
            let texto = v["result"]["content"][0]["text"].as_str().unwrap_or("").to_string();
            return if v["result"]["isError"] == serde_json::json!(true) { Err(texto) } else { Ok(texto) };
        }
    }
}

/// Um `orchestrator-mcp` por (projeto, pasta da workspace).
#[derive(Default)]
struct Sandboxes(Mutex<std::collections::HashMap<String, McpFilho>>);

fn chamar_sandbox(
    nucleo: &State<'_, Nucleo>,
    sandboxes: &State<'_, Sandboxes>,
    tool: &str,
    args: serde_json::Value,
) -> Result<String, String> {
    let env = {
        let e = travar(nucleo)?;
        e.cli_envs("dono", false)
    };
    let chave = env.iter().map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>().join(";");
    let mut mapa = sandboxes.0.lock().map_err(|_| "estado corrompido".to_string())?;
    if !mapa.contains_key(&chave) {
        mapa.insert(chave.clone(), McpFilho::novo(&env)?);
    }
    let r = mapa.get_mut(&chave).expect("inserido acima").chamar(tool, args);
    if matches!(&r, Err(e) if e.contains("fechou")) {
        mapa.remove(&chave);
    }
    r
}

/// O DONO sobe uma sandbox (mesma que as IAs usam): com endereço abre a
/// página; sem, só sobe o container. Pode demorar (o navegador sobe junto),
/// por isso é async — a janela não trava enquanto isso.
#[tauri::command]
async fn iniciar_sandbox(app: AppHandle, nome: String, url: String, docker: bool) -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let nucleo = app.state::<Nucleo>();
        let sandboxes = app.state::<Sandboxes>();
        if url.trim().is_empty() {
            let mut args = serde_json::json!({ "name": nome, "command": ["true"] });
            if docker {
                // `docker` só vale na criação, e quem cria sem página é o ui_exec
                // — então abre uma página em branco para passar pelo ui_open.
                args = serde_json::json!({ "name": nome, "url": "about:blank", "docker": true });
                return chamar_sandbox(&nucleo, &sandboxes, "ui_open", args);
            }
            chamar_sandbox(&nucleo, &sandboxes, "ui_exec", args)
        } else {
            let args = serde_json::json!({ "name": nome, "url": url, "docker": docker });
            chamar_sandbox(&nucleo, &sandboxes, "ui_open", args)
        }
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
async fn parar_sandbox(app: AppHandle, nome: String) -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let nucleo = app.state::<Nucleo>();
        let sandboxes = app.state::<Sandboxes>();
        chamar_sandbox(&nucleo, &sandboxes, "ui_stop", serde_json::json!({ "name": nome }))
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Resolve um caminho relativo dentro da raiz do projeto, recusando
/// qualquer coisa que escape dela (`..`, symlink apontando para fora) — o
/// IDE é do projeto, não do sistema de arquivos inteiro.
fn resolver_no_projeto(raiz: &std::path::Path, relativo: &str) -> Result<std::path::PathBuf, String> {
    let alvo = raiz.join(relativo);
    let canon = if alvo.exists() {
        alvo.canonicalize().map_err(|e| e.to_string())?
    } else {
        // Arquivo novo: canonicaliza o pai (que já existe) e reanexa o nome.
        let pai = alvo.parent().ok_or("caminho inválido")?;
        let nome = alvo.file_name().ok_or("caminho inválido")?;
        pai.canonicalize().map_err(|e| e.to_string())?.join(nome)
    };
    if !canon.starts_with(raiz) {
        return Err("fora da pasta do projeto".into());
    }
    Ok(canon)
}

/// A árvore de arquivos do projeto ativo, rasa (uma pasta por vez — quem
/// pede é a lateral do IDE, que expande sob demanda).
#[tauri::command]
fn ide_listar(nucleo: State<'_, Nucleo>, pasta: String) -> Result<Vec<EntradaArquivo>, String> {
    let raiz = raiz_projeto(&nucleo)?;
    let alvo = if pasta.trim().is_empty() { raiz.clone() } else { resolver_no_projeto(&raiz, &pasta)? };
    let mut entradas = Vec::new();
    let ler = std::fs::read_dir(&alvo).map_err(|e| e.to_string())?;
    for item in ler.flatten() {
        let nome = item.file_name().to_string_lossy().to_string();
        if nome.starts_with('.') && nome != ".env" || IDE_IGNORAR.contains(&nome.as_str()) {
            continue;
        }
        let Ok(tipo) = item.file_type() else { continue };
        let caminho_abs = item.path();
        let Ok(relativo) = caminho_abs.strip_prefix(&raiz) else { continue };
        entradas.push(EntradaArquivo {
            nome,
            caminho: relativo.to_string_lossy().replace('\\', "/"),
            pasta: tipo.is_dir(),
        });
    }
    entradas.sort_by(|a, b| b.pasta.cmp(&a.pasta).then(a.nome.to_lowercase().cmp(&b.nome.to_lowercase())));
    Ok(entradas)
}

/// Tamanho além do qual o IDE se recusa a abrir — texto maior que isso
/// quase sempre é gerado/binário, e travaria o editor no navegador.
const IDE_TAMANHO_MAX: u64 = 4 * 1024 * 1024;

#[tauri::command]
fn ide_ler(nucleo: State<'_, Nucleo>, caminho: String) -> Result<String, String> {
    let raiz = raiz_projeto(&nucleo)?;
    let alvo = resolver_no_projeto(&raiz, &caminho)?;
    let meta = std::fs::metadata(&alvo).map_err(|e| e.to_string())?;
    if meta.len() > IDE_TAMANHO_MAX {
        return Err("arquivo grande demais para abrir no IDE".into());
    }
    let bytes = std::fs::read(&alvo).map_err(|e| e.to_string())?;
    String::from_utf8(bytes).map_err(|_| "não é um arquivo de texto (provavelmente binário)".into())
}

#[tauri::command]
fn ide_salvar(nucleo: State<'_, Nucleo>, caminho: String, conteudo: String) -> Result<(), String> {
    let raiz = raiz_projeto(&nucleo)?;
    let alvo = resolver_no_projeto(&raiz, &caminho)?;
    std::fs::write(&alvo, conteudo).map_err(|e| e.to_string())
}

#[tauri::command]
fn trocar_workspace(nucleo: State<'_, Nucleo>, indice: usize) -> Result<(), String> {
    travar(&nucleo)?.switch_workspace(indice);
    Ok(())
}

#[tauri::command]
fn focar_card(nucleo: State<'_, Nucleo>, indice: usize) -> Result<(), String> {
    let mut e = travar(&nucleo)?;
    let ws = e.ws_idx;
    if indice < e.workspaces[ws].panes.len() {
        e.workspaces[ws].focused = indice;
    }
    Ok(())
}

#[tauri::command]
fn fechar_card(app: AppHandle, nucleo: State<'_, Nucleo>, indice: usize) -> Result<(), String> {
    let eventos = {
        let mut e = travar(&nucleo)?;
        let ws = e.ws_idx;
        if indice >= e.workspaces[ws].panes.len() {
            return Err("esse card não existe mais".into());
        }
        e.workspaces[ws].focused = indice;
        e.close_focused_pane();
        e.save_workspace_clis();
        e.take_events()
    };
    avisar_pedidos(&app, eventos);
    Ok(())
}

#[tauri::command]
fn resolver_decisao(nucleo: State<'_, Nucleo>, id: String, aprovar: bool) -> Result<(), String> {
    travar(&nucleo)?.resolve_by_chat(aprovar, Some(&id));
    Ok(())
}

/// O dono responde a uma pergunta do orquestrador (alternativas já em texto).
#[tauri::command]
fn responder_pergunta(nucleo: State<'_, Nucleo>, id: String, resposta: String) -> Result<(), String> {
    travar(&nucleo)?.answer_question(&id, &resposta)
}

/// Liga o xterm.js a uma CLI: primeiro a tela atual, depois cada pedaço de
/// saída, em ordem, até a CLI acabar ou a janela soltar o canal.
#[tauri::command]
fn assinar_terminal(
    nucleo: State<'_, Nucleo>,
    indice: usize,
    canal: Channel<InvokeResponseBody>,
) -> Result<(), String> {
    let (foto, rx) = {
        let e = travar(&nucleo)?;
        match e.workspaces[e.ws_idx].panes.get(indice) {
            Some(Pane::Term(t)) => t.subscribe_output(),
            _ => return Err("esse card não é um terminal".into()),
        }
    };
    canal
        .send(InvokeResponseBody::Raw(foto))
        .map_err(|e| format!("canal fechado: {e}"))?;
    std::thread::spawn(move || {
        while let Ok(pedaco) = rx.recv() {
            if canal.send(InvokeResponseBody::Raw(pedaco)).is_err() {
                break;
            }
        }
    });
    Ok(())
}

#[tauri::command]
fn escrever_terminal(nucleo: State<'_, Nucleo>, indice: usize, dados: String) -> Result<(), String> {
    let mut e = travar(&nucleo)?;
    let ws = e.ws_idx;
    match e.workspaces[ws].panes.get_mut(indice) {
        Some(Pane::Term(t)) => {
            t.write_bytes(dados.as_bytes());
            Ok(())
        }
        _ => Err("esse card não é um terminal".into()),
    }
}

#[tauri::command]
fn redimensionar_terminal(
    nucleo: State<'_, Nucleo>,
    indice: usize,
    linhas: u16,
    colunas: u16,
) -> Result<(), String> {
    let mut e = travar(&nucleo)?;
    let ws = e.ws_idx;
    if let Some(Pane::Term(t)) = e.workspaces[ws].panes.get_mut(indice) {
        t.resize(linhas, colunas);
    }
    Ok(())
}

/// Mensagem de acompanhamento para um agente headless (vai para a fila se
/// ele estiver no meio de um turno).
#[tauri::command]
fn iterar_agente(nucleo: State<'_, Nucleo>, indice: usize, texto: String) -> Result<(), String> {
    let mut e = travar(&nucleo)?;
    let ws = e.ws_idx;
    match e.workspaces[ws].panes.get_mut(indice) {
        Some(Pane::Agent(a)) => {
            if a.busy {
                a.queue_followup(texto);
            } else {
                a.send_followup(texto);
            }
            Ok(())
        }
        _ => Err("esse card não é um agente".into()),
    }
}

/// Onde a API da memória está (o painel de memória fala direto com ela).
#[tauri::command]
fn memoria_api() -> String {
    let arquivo = orchestrator_memory::daemon::socket_path().with_file_name("memory-api.port");
    let porta = std::fs::read_to_string(arquivo)
        .ok()
        .and_then(|p| p.trim().parse::<u16>().ok())
        .unwrap_or(10000);
    format!("http://127.0.0.1:{porta}")
}

// ------------------------------------------------------------------ laço

/// O mesmo trabalho do laço da TUI, sem desenhar: quem desenha é a janela.
// ------------------------------------------------------ acesso remoto

/// Define a senha do acesso remoto (guardada com hash+sal, nunca em texto).
#[tauri::command]
fn remoto_definir_senha(nucleo: State<'_, Nucleo>, senha: String) -> Result<(), String> {
    let e = travar(&nucleo)?;
    remoto::definir_senha(&e.store, &senha)
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct RemotoStatus {
    senha_definida: bool,
    /// 2FA (autenticador) ativo?
    totp_ativo: bool,
    /// URL base do túnel enquanto ligado (`null` desligado).
    url: Option<String>,
    /// Caminho SECRETO da tela de login (`/entrar/<token>`) — junto com a URL
    /// forma o link completo. Quem não tem o token recebe 404.
    caminho: String,
    /// Histórico recente de acessos (ok/falha, hora, IP), para o dono ver.
    acessos: Vec<remoto::Evento>,
}

#[tauri::command]
fn remoto_status(nucleo: State<'_, Nucleo>, remoto: State<'_, remoto::Remoto>) -> Result<RemotoStatus, String> {
    let (senha_definida, totp_ativo, caminho) = {
        let e = travar(&nucleo)?;
        (
            remoto::senha_definida(&e.store),
            remoto::totp_ativo(&e.store),
            format!("/entrar/{}", remoto::gate_token(&e.store)),
        )
    };
    let acessos = remoto.protecao().lock().map(|p| p.eventos()).unwrap_or_default();
    Ok(RemotoStatus { senha_definida, totp_ativo, url: remoto.url(), caminho, acessos })
}

/// Gera um link de acesso novo (invalida o antigo) e derruba as sessões.
#[tauri::command]
fn remoto_regenerar_token(nucleo: State<'_, Nucleo>, remoto: State<'_, remoto::Remoto>) -> Result<String, String> {
    let caminho = {
        let e = travar(&nucleo)?;
        format!("/entrar/{}", remoto::regenerar_token(&e.store))
    };
    remoto.revogar_sessoes();
    Ok(caminho)
}

#[tauri::command]
fn remoto_revogar_sessoes(remoto: State<'_, remoto::Remoto>) -> Result<(), String> {
    remoto.revogar_sessoes();
    Ok(())
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct TotpInicio {
    otpauth: String,
    secret: String,
}

/// Começa a configurar o 2FA: cria o segredo e devolve o `otpauth://` (para
/// o app autenticador) + o segredo em base32 (para digitar à mão).
#[tauri::command]
fn remoto_totp_iniciar(nucleo: State<'_, Nucleo>) -> Result<TotpInicio, String> {
    let e = travar(&nucleo)?;
    let (otpauth, secret) = remoto::totp_iniciar(&e.store);
    Ok(TotpInicio { otpauth, secret })
}

/// Confirma o autenticador com um código atual e liga o 2FA.
#[tauri::command]
fn remoto_totp_ativar(nucleo: State<'_, Nucleo>, codigo: String) -> Result<(), String> {
    let e = travar(&nucleo)?;
    remoto::totp_ativar(&e.store, &codigo)
}

#[tauri::command]
fn remoto_totp_desativar(nucleo: State<'_, Nucleo>) -> Result<(), String> {
    let e = travar(&nucleo)?;
    remoto::totp_desativar(&e.store);
    Ok(())
}

/// Liga o túnel Cloudflare (sobe o servidor local se preciso) e devolve a URL.
/// Bloqueia até o cloudflared responder, então roda fora do executor async.
#[tauri::command]
async fn remoto_ligar(app: AppHandle) -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let remoto = app.state::<remoto::Remoto>();
        remoto.ligar(&app)
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
fn remoto_desligar(remoto: State<'_, remoto::Remoto>) -> Result<(), String> {
    remoto.desligar()
}

fn laco(app: AppHandle) {
    let mut ultima: Option<String> = None;
    loop {
        let (texto, eventos) = {
            let nucleo = app.state::<Nucleo>();
            let Ok(mut e) = nucleo.0.lock() else { return };
            if e.last_reload.elapsed() >= RELOAD_EVERY {
                e.reload();
            }
            e.drain_background();
            let f = foto(&e);
            (serde_json::to_string(&f).ok(), e.take_events())
        };
        avisar_pedidos(&app, eventos);
        if texto.is_some() && texto != ultima {
            if let Some(t) = &texto {
                let _ = app.emit("estado", serde_json::from_str::<serde_json::Value>(t).ok());
            }
            ultima = texto;
        }
        std::thread::sleep(TICK);
    }
}

fn abrir_nucleo() -> anyhow::Result<Engine> {
    orchestrator_memory::daemon::ensure_started();
    let caminho = config::default_config_path()?;
    let cfg = Config::load(&caminho)?;
    let store = MemoryStore::open(&cfg.memory_db_path)?;
    let mut engine = Engine::new(store, &cfg, cfg.memory_db_path.clone());
    engine.config_path = Some(caminho);
    engine.refresh_notices();
    Ok(engine)
}

/// O WebKitGTK derruba a janela no Wayland ("Error 71 dispatching to Wayland
/// display") com o renderizador DMABUF — medido aqui (Fedora/KDE, AMD). Sem
/// ele a janela abre normal; quem já definiu a variável manda.
#[cfg(target_os = "linux")]
fn contornar_webkit_wayland() {
    if std::env::var_os("WEBKIT_DISABLE_DMABUF_RENDERER").is_none() {
        // Antes de qualquer thread do GTK existir.
        unsafe { std::env::set_var("WEBKIT_DISABLE_DMABUF_RENDERER", "1") };
    }
}

pub fn run() {
    #[cfg(target_os = "linux")]
    contornar_webkit_wayland();
    let inicio = Instant::now();
    let engine = match abrir_nucleo() {
        Ok(e) => e,
        Err(err) => {
            eprintln!("Orchestrator: não consegui abrir o núcleo: {err:#}");
            std::process::exit(1);
        }
    };
    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            // Abrir de novo só traz a janela que já existe para a frente.
            if let Some(janela) = app.get_webview_window("main") {
                let _ = janela.set_focus();
            }
        }))
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .manage(Nucleo(Arc::new(Mutex::new(engine))))
        .manage(Sandboxes::default())
        .manage(remoto::Remoto::default())
        .setup(move |app| {
            let handle = app.handle().clone();
            std::thread::spawn(move || laco(handle));
            eprintln!("Orchestrator pronto em {} ms", inicio.elapsed().as_millis());
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            estado,
            enviar,
            paleta,
            provedores,
            escolher_provedor,
            sincronizar_modelos,
            testar_modelo,
            tela_viva,
            ide_listar,
            ide_ler,
            ide_salvar,
            alterar_pasta_projeto,
            alterar_pasta_workspace,
            abrir_terminal,
            abrir_ssh,
            ssh_listar,
            ssh_salvar,
            iniciar_sandbox,
            parar_sandbox,
            remoto_definir_senha,
            remoto_status,
            remoto_ligar,
            remoto_desligar,
            remoto_regenerar_token,
            remoto_revogar_sessoes,
            remoto_totp_iniciar,
            remoto_totp_ativar,
            remoto_totp_desativar,
            trocar_workspace,
            focar_card,
            fechar_card,
            resolver_decisao,
            responder_pergunta,
            assinar_terminal,
            escrever_terminal,
            redimensionar_terminal,
            iterar_agente,
            memoria_api,
        ])
        .run(tauri::generate_context!())
        .expect("erro ao rodar o app do Orchestrator");
}
