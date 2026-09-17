//! Tools `ui_*`: o orquestrador testando o que as CLIs produziram.
//!
//! A sandbox vive no processo do MCP (não na TUI): diferente das CLIs em
//! PTY, aqui não há tela para desenhar — é tudo container e navegador, então
//! não precisa passar pela fila.
//!
//! Todas as respostas terminam com o mesmo lembrete (ver
//! `orchestrator_sandbox::view::HINT`): como agir por referência e que dá
//! para pedir o screenshot quando o texto não bastar.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

use orchestrator_sandbox::container::{self, Mount, SandboxSpec};
use orchestrator_sandbox::view::Detail;
use orchestrator_sandbox::Session;
use serde_json::{json, Value};

/// Chave de uma sandbox no mapa de sessões.
///
/// É o nome do container, não o nome cru: "verificação" e "verificacao"
/// viram o MESMO container, e duas sessões apontando para ele davam um
/// `ui_stop` que matava o container da outra.
fn chave(name: &str) -> String {
    container::sanitize(name)
}

/// Sandboxes abertas neste processo, por nome de container.
fn sessions() -> &'static Mutex<HashMap<String, Session>> {
    static S: OnceLock<Mutex<HashMap<String, Session>>> = OnceLock::new();
    S.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Onde os screenshots são gravados (o orquestrador recebe o caminho).
fn shots_dir() -> PathBuf {
    std::env::var_os("ORCHESTRATOR_SHOTS")
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("orchestrator-shots"))
}

/// Onde a "tela virtual" (captura contínua, um arquivo por sandbox) fica.
/// Separada de `shots_dir()` para não se misturar com os screenshots que o
/// orquestrador pede um a um via `ui_screenshot`.
fn live_dir() -> PathBuf {
    shots_dir().join("live")
}

/// Um sinalizador "continue capturando" por sandbox — a thread de captura o
/// olha a cada volta; `ui_stop` desliga e a thread sai sozinha, sem precisar
/// derrubar nada à força.
fn live_flags() -> &'static Mutex<HashMap<String, std::sync::Arc<std::sync::atomic::AtomicBool>>> {
    static F: OnceLock<Mutex<HashMap<String, std::sync::Arc<std::sync::atomic::AtomicBool>>>> =
        OnceLock::new();
    F.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Sobe a thread de captura da "tela virtual" de uma sandbox recém-criada.
///
/// Cada volta tira uma foto (sobrescrevendo sempre o mesmo arquivo, ver
/// `Session::screenshot_to`) e grava o caminho + hora no `ui_state` — a
/// mesma tabela que o `Engine` já lê para tudo o mais (sessão de chat,
/// pasta de cada workspace...), então o app só precisa de mais um
/// `ui_get`. Falha de captura (página ainda não aberta, navegador ainda de
/// pé subindo) é normal no começo e não derruba a thread.
fn iniciar_tela_viva(chave: String) {
    let ligado = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
    live_flags()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(chave.clone(), ligado.clone());
    let db_path = std::env::var_os("ORCHESTRATOR_DB").map(PathBuf::from);
    std::thread::spawn(move || {
        let store = match crate::open_store(db_path) {
            Ok(s) => s,
            Err(_) => return,
        };
        let dir = live_dir();
        if std::fs::create_dir_all(&dir).is_err() {
            return;
        }
        let path = dir.join(format!("{chave}.png"));
        use std::sync::atomic::Ordering;
        while ligado.load(Ordering::Relaxed) {
            std::thread::sleep(std::time::Duration::from_millis(1500));
            if !ligado.load(Ordering::Relaxed) {
                break;
            }
            let mut map = match sessions().lock() {
                Ok(m) => m,
                Err(_) => break,
            };
            let Some(session) = map.get_mut(&chave) else {
                break;
            };
            if session.screenshot_to(&path).is_ok() {
                let _ = store.ui_set(&format!("sandbox.{chave}.tela_viva"), &path.display().to_string());
                let _ = store.ui_set(
                    &format!("sandbox.{chave}.tela_viva_em"),
                    &chrono::Utc::now().to_rfc3339(),
                );
            }
        }
        let _ = store.ui_delete(&format!("sandbox.{chave}.tela_viva"));
    });
}

/// Desliga a captura da "tela virtual" de uma sandbox (chamado por
/// `ui_stop`, antes de derrubar o container — a thread nota no próximo
/// laço, no máximo 1.5s depois, e sai sozinha).
fn parar_tela_viva(chave: &str) {
    if let Some(f) = live_flags().lock().unwrap_or_else(|e| e.into_inner()).remove(chave) {
        f.store(false, std::sync::atomic::Ordering::Relaxed);
    }
}

/// Descritores das tools de sandbox para o `tools/list`.
pub fn tool_descriptors() -> Vec<Value> {
    vec![
        json!({
            "name": "ui_open",
            "description": "Abre uma SANDBOX de teste (container isolado com navegador e tela virtual) e carrega um endereço. Sem `workdir`, monta a pasta da workspace atual em /work — normalmente é o que você quer. Dentro da sandbox a escrita é LIVRE e DESCARTADA no fim: o teste pode criar arquivo, banco e log sem alterar o projeto do usuário. É assim que você VERIFICA o que uma CLI produziu, sem tocar na máquina nem na tela do usuário. Devolve a página como lista de elementos com referência ([e1], [e2]...) — não como imagem.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "name": { "type": "string", "description": "Nome da sandbox (ex.: \"teste-frontend\")." },
                    "url": { "type": "string", "description": "Endereço ou arquivo a abrir (ex.: \"localhost:3000\", \"/work/index.html\")." },
                    "workdir": { "type": "string", "description": "Pasta do host a montar em /work. Padrão: a pasta que a TUI publicou para a workspace ativa — se não houver nenhuma, /work vem VAZIO e você precisa passar esta pasta." },
                    "writable": { "type": "boolean", "description": "Gravar DE VERDADE na pasta do host (padrão: false). Você quase nunca precisa disto: por padrão a sandbox já escreve à vontade, só numa cópia descartável. Use true apenas quando o resultado tiver de ficar no projeto." }
                },
                "required": ["name", "url"]
            }
        }),
        json!({
            "name": "ui_snapshot",
            "description": "Relê a página da sandbox. Por padrão mostra só o que MUDOU desde o passo anterior (barato); passe full=true para relistar todos os elementos.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "name": { "type": "string" },
                    "full": { "type": "boolean", "description": "Listar tudo em vez de só as mudanças." }
                },
                "required": ["name"]
            }
        }),
        json!({
            "name": "ui_click",
            "description": "Clica num elemento pela referência do último snapshot (ex.: \"e3\"). Devolve o que mudou na página depois do clique.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "name": { "type": "string" },
                    "ref": { "type": "string", "description": "Referência do elemento, ex.: \"e3\"." }
                },
                "required": ["name", "ref"]
            }
        }),
        json!({
            "name": "ui_type",
            "description": "Escreve num campo pela referência (ex.: \"e5\"), disparando os eventos que frameworks web esperam. Devolve o que mudou.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "name": { "type": "string" },
                    "ref": { "type": "string" },
                    "text": { "type": "string" }
                },
                "required": ["name", "ref", "text"]
            }
        }),
        json!({
            "name": "ui_select",
            "description": "Escolhe um valor num <select> pela referência.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "name": { "type": "string" },
                    "ref": { "type": "string" },
                    "value": { "type": "string" }
                },
                "required": ["name", "ref", "value"]
            }
        }),
        json!({
            "name": "ui_screenshot",
            "description": "Tira uma foto da tela da sandbox. A imagem é GRAVADA EM ARQUIVO e você recebe só o caminho — abra com a tool Read apenas se realmente precisar ver (layout quebrado, cor errada, algo que o texto não conta). Peça isto quando os elementos não bastarem.",
            "inputSchema": {
                "type": "object",
                "properties": { "name": { "type": "string" } },
                "required": ["name"]
            }
        }),
        json!({
            "name": "ui_exec",
            "description": "Roda um comando DENTRO da sandbox — é como você testa um binário, um AppImage, um servidor ou um script gerado por uma CLI, sem risco para a máquina do usuário. Se a sandbox ainda não existir, ela SOBE SOZINHA na pasta da workspace: não precisa de ui_open antes (a ordem natural é subir o serviço aqui e só depois abrir a página). Por padrão a escrita em /work é descartada no fim, então pode gravar (exceto numa sandbox aberta com writable=true, que grava na pasta real — nesse caso toda resposta avisa). ATENÇÃO: o comando roda em PRIMEIRO PLANO e esta chamada só responde quando ele terminar (prazo de 120s) — serviço que não termina sozinho tem de ir para segundo plano: [\"sh\",\"-c\",\"(setsid python3 servidor.py >/tmp/srv.log 2>&1 &); sleep 2; curl -s localhost:8080 | head -3\"]. Devolve a saída (cortada nas últimas linhas).",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "name": { "type": "string" },
                    "command": { "type": "array", "items": { "type": "string" }, "description": "Comando e argumentos, ex.: [\"./app.AppImage\", \"--version\"]. Para subir serviço, mande para segundo plano e confira em seguida (ver a descrição da tool)." }
                },
                "required": ["name", "command"]
            }
        }),
        json!({
            "name": "ui_stop",
            "description": "Encerra a sandbox (o container é descartado, e com ele tudo que foi escrito lá dentro). Sem \"name\", lista as sandboxes abertas e em que pasta cada uma está.",
            "inputSchema": {
                "type": "object",
                "properties": { "name": { "type": "string" } }
            }
        }),
    ]
}

/// Os nomes que este módulo atende.
pub fn handles(name: &str) -> bool {
    name.starts_with("ui_")
}

/// Executa uma tool `ui_*`, registrando o que rodou.
///
/// O registro existe para CONFERIR: sem ele, "testei na sandbox" não tinha
/// como ser verificado — as tools de sandbox agem direto, sem passar pela
/// fila que dá rastro às de CLI.
pub fn call_logged(
    store: &orchestrator_memory::store::MemoryStore,
    project: &str,
    name: &str,
    args: &Value,
) -> Result<String, (i64, String)> {
    let resultado = call(name, args);
    let (texto, ok) = match &resultado {
        Ok(t) => (t.clone(), true),
        Err((_, e)) => (e.clone(), false),
    };
    let _ = store.log_tool_call(project, name, &args.to_string(), &texto, ok);
    resultado
}

/// Executa uma tool `ui_*` e devolve o texto para o orquestrador.
pub fn call(name: &str, args: &Value) -> Result<String, (i64, String)> {
    let err = |e: anyhow::Error| (-32000i64, format!("{e:#}"));
    match name {
        "ui_open" => {
            let sandbox = str_arg(args, "name")?;
            let url = str_arg(args, "url")?;
            let mut spec = spec_padrao(sandbox);
            if let Some(dir) = args
                .get("workdir")
                .and_then(Value::as_str)
                .filter(|s| !s.trim().is_empty())
            {
                spec.workdir = Some(PathBuf::from(dir));
            }
            // `writable` só existe para o caso raro de o resultado precisar
            // FICAR na pasta do usuário; o padrão já deixa escrever.
            if args
                .get("writable")
                .and_then(Value::as_bool)
                .unwrap_or(false)
            {
                spec.mount = Mount::ReadWrite;
            }

            let k = chave(sandbox);
            let mut map = sessions().lock().map_err(|_| lock_err())?;
            let mut aviso = None;
            // A sandbox desta sessão pode ter nascido em outra pasta ou em
            // outro modo (um `ui_exec` anterior, por exemplo) — e nem a
            // pasta nem o modo mudam com o container vivo.
            if let Some(viva) = map.get(&k) {
                container::serve(&viva.sandbox, &spec).map_err(err)?;
            }
            if !map.contains_key(&k) {
                let session = Session::start(&spec, &shots_dir()).map_err(err)?;
                aviso = Some(session.resumo());
                map.insert(k.clone(), session);
                iniciar_tela_viva(k.clone());
            }
            let session = map.get_mut(&k).expect("inserida acima");
            let alerta = alerta_de_escrita_real(&session.sandbox);
            let pagina = session.open(url).map_err(err)?;
            Ok(com_aviso(pagina, aviso.or(alerta)))
        }
        "ui_snapshot" => {
            let sandbox = str_arg(args, "name")?;
            let detail = if args.get("full").and_then(Value::as_bool).unwrap_or(false) {
                Detail::Full
            } else {
                Detail::Changes
            };
            texto_da_sandbox(sandbox, "ui_snapshot", |s| s.snapshot(detail)).map_err(err)
        }
        "ui_click" => {
            let sandbox = str_arg(args, "name")?;
            let reference = str_arg(args, "ref")?.to_string();
            texto_da_sandbox(sandbox, "ui_click", |s| s.act("click", &reference, "")).map_err(err)
        }
        "ui_type" => {
            let sandbox = str_arg(args, "name")?;
            let reference = str_arg(args, "ref")?.to_string();
            let text = str_arg(args, "text")?.to_string();
            texto_da_sandbox(sandbox, "ui_type", |s| s.act("type", &reference, &text)).map_err(err)
        }
        "ui_select" => {
            let sandbox = str_arg(args, "name")?;
            let reference = str_arg(args, "ref")?.to_string();
            let value = str_arg(args, "value")?.to_string();
            texto_da_sandbox(sandbox, "ui_select", |s| s.act("select", &reference, &value)).map_err(err)
        }
        "ui_screenshot" => {
            let sandbox = str_arg(args, "name")?;
            // Fotografar exige página aberta, então aqui nunca há aviso.
            let (path, _) =
                session_for(sandbox, "ui_screenshot", |s| s.screenshot()).map_err(err)?;
            Ok(format!(
                "tela salva em {}\nAbra com a tool Read SÓ se precisar ver a imagem — \
                 para saber o que está na página, ui_snapshot custa muito menos.",
                path.display()
            ))
        }
        "ui_exec" => {
            let sandbox = str_arg(args, "name")?;
            let argv: Vec<String> = args
                .get("command")
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(|v| v.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default();
            if argv.is_empty() {
                return Err((-32602, "informe o comando a rodar na sandbox".into()));
            }
            texto_da_sandbox(sandbox, "ui_exec", |s| s.exec(&argv)).map_err(err)
        }
        "ui_stop" => match args.get("name").and_then(Value::as_str) {
            Some(sandbox) => {
                let k = chave(sandbox);
                parar_tela_viva(&k);
                let mut map = sessions().lock().map_err(|_| lock_err())?;
                let estava = match map.remove(&k) {
                    Some(mut s) => s.stop().map_err(err)?,
                    // Pode ter sobrado de outra execução do MCP.
                    None => container::stop(sandbox).map_err(err)?,
                };
                if estava {
                    return Ok(format!("sandbox \"{sandbox}\" encerrada"));
                }
                // Nome digitado errado é o caso comum aqui. Dizer "já estava
                // encerrada" faria ele marcar a limpeza como feita enquanto a
                // sandbox de verdade segue de pé — com navegador e, se for o
                // caso, com a pasta do usuário montada.
                let abertas = container::list().unwrap_or_default();
                Ok(if abertas.is_empty() {
                    format!("não há sandbox \"{sandbox}\" de pé, e nenhuma outra aberta")
                } else {
                    format!(
                        "não há sandbox \"{sandbox}\" de pé. Abertas agora:\n{}",
                        lista_abertas(&abertas)
                    )
                })
            }
            None => {
                let abertas = container::list().map_err(err)?;
                if abertas.is_empty() {
                    Ok("nenhuma sandbox aberta — ui_open cria uma".to_string())
                } else {
                    Ok(lista_abertas(&abertas))
                }
            }
        },
        other => Err((-32602, format!("tool de sandbox desconhecida: {other}"))),
    }
}

/// As sandboxes de pé, uma por linha, com pasta e modo.
///
/// O modo aparece porque é o que distingue uma sandbox descartável de uma
/// aberta com escrita real — quem readota precisa disso antes de gravar.
fn lista_abertas(abertas: &[container::Aberta]) -> String {
    abertas
        .iter()
        .map(|a| {
            let mut linha = format!("- {} (de pé há {}", a.name, a.since);
            if let Some(d) = &a.workdir {
                linha.push_str(&format!(", /work = {}", d.display()));
            }
            if let Some(m) = a.mount {
                linha.push_str(&format!(", {}", m.descricao()));
            }
            linha.push(')');
            linha
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// O aviso que acompanha TODA resposta de uma sandbox que grava de verdade.
///
/// O aviso de criação não bastava: ele sai uma vez só, e muitos turnos
/// depois a tool seguinte não tem como saber que `/work` é a pasta real do
/// usuário — um `rm -rf /work/*` ali não é descartável.
fn alerta_de_escrita_real(s: &orchestrator_sandbox::Sandbox) -> Option<String> {
    (s.mount == Mount::ReadWrite && s.workdir.is_some()).then(|| {
        format!(
            "⚠ nesta sandbox /work É a pasta real do usuário ({}) — o que você \
             escrever FICA lá.",
            s.workdir.as_ref().expect("checado acima").display()
        )
    })
}

/// Sandbox pedida só pelo nome: a pasta da workspace ativa, escrita
/// descartável.
///
/// A TUI publica a pasta em `ORCHESTRATOR_WORKDIR`; sem isso o orquestrador
/// precisava adivinhar o caminho e a sandbox abria vazia.
fn spec_padrao(name: &str) -> SandboxSpec {
    let mut spec = SandboxSpec::new(name);
    spec.workdir = std::env::var_os("ORCHESTRATOR_WORKDIR").map(PathBuf::from);
    spec
}

/// Tools que podem SUBIR a sandbox sozinhas.
///
/// Só entram aqui as que não dependem de página aberta. A ordem natural de
/// um teste é subir o serviço (`ui_exec`) e só então abrir a página — exigir
/// `ui_open` antes gastava um passo carregando uma página vazia, que foi
/// exatamente o que aconteceu no teste ao vivo. Já agir por referência
/// (`e3`) numa sandbox recém-nascida não significa nada: ali a resposta
/// certa continua sendo mandar o orquestrador chamar `ui_open`.
fn abre_sozinha(tool: &str) -> bool {
    matches!(tool, "ui_exec")
}

/// Pega a sessão da sandbox e roda `f` nela, subindo-a se a tool permitir.
///
/// Devolve também um aviso quando a sandbox nasceu (ou foi readotada) agora:
/// o orquestrador precisa saber em que pasta acabou de mexer.
fn session_for<T>(
    name: &str,
    tool: &str,
    f: impl FnOnce(&mut Session) -> anyhow::Result<T>,
) -> anyhow::Result<(T, Option<String>)> {
    let k = chave(name);
    let mut map = sessions()
        .lock()
        .map_err(|_| anyhow::anyhow!("estado da sandbox corrompido"))?;
    let mut aviso = None;
    if !map.contains_key(&k) {
        if !abre_sozinha(tool) {
            anyhow::bail!("nenhuma sandbox chamada \"{name}\" — abra com ui_open");
        }
        let session = Session::start(&spec_padrao(name), &shots_dir())?;
        aviso = Some(session.resumo());
        map.insert(k.clone(), session);
    }
    let session = map.get_mut(&k).expect("presente acima");
    // O resumo da criação já diz o modo; fora dele, o alerta é o que mantém
    // visível que aquela sandbox grava na pasta real.
    if aviso.is_none() {
        aviso = alerta_de_escrita_real(&session.sandbox);
    }
    let saida = f(session)?;
    Ok((saida, aviso))
}

/// Igual a [`session_for`], para as tools que já devolvem texto pronto.
fn texto_da_sandbox(
    name: &str,
    tool: &str,
    f: impl FnOnce(&mut Session) -> anyhow::Result<String>,
) -> anyhow::Result<String> {
    let (texto, aviso) = session_for(name, tool, f)?;
    Ok(com_aviso(texto, aviso))
}

/// Põe o aviso da sandbox na frente da resposta, quando houver.
fn com_aviso(texto: String, aviso: Option<String>) -> String {
    match aviso {
        Some(a) => format!("{a}\n{texto}"),
        None => texto,
    }
}

fn lock_err() -> (i64, String) {
    (-32000, "estado da sandbox corrompido".into())
}

fn str_arg<'a>(args: &'a Value, key: &str) -> Result<&'a str, (i64, String)> {
    args.get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
        .ok_or((-32602, format!("argumento obrigatório ausente: {key}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn descriptors_cover_the_whole_flow() {
        let names: Vec<String> = tool_descriptors()
            .iter()
            .map(|t| t["name"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(
            names,
            [
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
        assert!(names.iter().all(|n| handles(n)));
        assert!(!handles("cli_start"));
    }

    #[test]
    fn screenshot_description_teaches_the_cheap_path_first() {
        let d = tool_descriptors()
            .into_iter()
            .find(|t| t["name"] == "ui_screenshot")
            .unwrap();
        let desc = d["description"].as_str().unwrap();
        // O ponto do desenho: imagem é o caro, e ele precisa saber disso.
        assert!(desc.contains("ARQUIVO"));
        assert!(desc.contains("ui_snapshot") || desc.contains("Read"));
    }

    #[test]
    fn acting_without_a_sandbox_explains_what_to_do() {
        let err = call(
            "ui_click",
            &json!({ "name": "nao-existe", "ref": "e1" }),
        )
        .unwrap_err();
        assert!(err.1.contains("ui_open"), "{}", err.1);
    }

    #[test]
    fn missing_arguments_are_reported_by_name() {
        let err = call("ui_open", &json!({ "name": "x" })).unwrap_err();
        assert!(err.1.contains("url"));
        let err = call("ui_exec", &json!({ "name": "x" })).unwrap_err();
        assert!(err.1.contains("comando"));
        let err = call("ui_click", &json!({ "name": "  ", "ref": "e1" })).unwrap_err();
        assert!(err.1.contains("name"));
    }

    #[test]
    fn every_call_leaves_a_trace_even_when_it_fails() {
        use orchestrator_memory::store::MemoryStore;

        let store = MemoryStore::open_in_memory().unwrap();
        // Falha (não há sandbox aberta) — mesmo assim tem de ficar registrado.
        let r = call_logged(
            &store,
            "p",
            "ui_click",
            &json!({ "name": "fantasma", "ref": "e1" }),
        );
        assert!(r.is_err());

        let calls = store.recent_tool_calls("p", 10).unwrap();
        assert_eq!(calls.len(), 1, "a chamada precisa aparecer no rastro");
        assert_eq!(calls[0].0, "ui_click");
        assert!(calls[0].1.contains("fantasma"), "argumentos: {}", calls[0].1);
        assert!(!calls[0].3, "foi uma falha");
        assert!(calls[0].2.contains("ui_open"), "guardou o motivo: {}", calls[0].2);
    }

    #[test]
    fn only_exec_opens_a_sandbox_by_itself() {
        // Subir o serviço é o primeiro passo de um teste: não pode exigir
        // que uma página tenha sido aberta antes.
        assert!(abre_sozinha("ui_exec"));
        // Agir por referência numa sandbox recém-nascida não significa nada.
        for t in ["ui_click", "ui_type", "ui_select", "ui_snapshot", "ui_screenshot"] {
            assert!(!abre_sozinha(t), "{t} não pode nascer sozinha");
        }
    }

    #[test]
    fn exec_says_it_does_not_need_ui_open_first() {
        let d = tool_descriptors()
            .into_iter()
            .find(|t| t["name"] == "ui_exec")
            .unwrap();
        let desc = d["description"].as_str().unwrap();
        assert!(desc.contains("ui_open"), "{desc}");
        assert!(desc.to_lowercase().contains("sozinha"), "{desc}");
    }

    #[test]
    fn open_teaches_that_writing_does_not_touch_the_project() {
        let tools = tool_descriptors();
        let abrir = tools.iter().find(|t| t["name"] == "ui_open").unwrap();
        let desc = abrir["description"].as_str().unwrap();
        // A queixa que isto resolve: com `:ro` o teste morria em
        // "read-only file system" e ele copiava tudo para /tmp.
        assert!(desc.contains("DESCARTADA"), "{desc}");
        let writable = abrir["inputSchema"]["properties"]["writable"]["description"]
            .as_str()
            .unwrap();
        assert!(writable.contains("host"), "{writable}");
    }

    #[test]
    fn the_default_spec_follows_the_workspace_folder() {
        // A TUI publica a pasta da workspace; a sandbox tem de cair nela.
        std::env::set_var("ORCHESTRATOR_WORKDIR", "/tmp/uma-workspace");
        let spec = spec_padrao("x");
        assert_eq!(spec.workdir, Some(PathBuf::from("/tmp/uma-workspace")));
        // E escrever ali não pode alterar o projeto do usuário.
        assert_eq!(spec.mount, Mount::Overlay);
        std::env::remove_var("ORCHESTRATOR_WORKDIR");
    }

    #[test]
    fn two_spellings_of_one_name_are_the_same_sandbox() {
        // O container só aceita o nome sanitizado, então nomes que só
        // diferem em espaço/pontuação são o MESMO container: duas sessões
        // apontando para ele davam um ui_stop que matava a sandbox da outra.
        assert_eq!(chave("Meu Teste"), chave("meu-teste"));
        assert_eq!(chave("teste!"), chave("teste"));
        assert_eq!(chave("frontend."), chave("frontend"));
        // Acento NÃO colapsa: cada caractere fora do ASCII vira um '-'
        // próprio, então "verificação" e "verificacao" são sandboxes
        // distintas (é o teto de sandboxes que segura essa confusão).
        assert_ne!(chave("verificação"), chave("verificacao"));
        assert_ne!(chave("frontend"), chave("backend"));
    }

    #[test]
    fn a_sandbox_that_writes_for_real_says_so_every_time() {
        use orchestrator_sandbox::container::Mount;
        let mut s = orchestrator_sandbox::Sandbox {
            name: "t".into(),
            container: "orch-sbx-t".into(),
            cdp_url: "http://127.0.0.1:5000".into(),
            port: 5000,
            workdir: Some(PathBuf::from("/home/eu/projeto")),
            mount: Mount::ReadWrite,
            adopted: false,
        };
        let a = alerta_de_escrita_real(&s).expect("escrita real precisa avisar");
        assert!(a.contains("/home/eu/projeto"), "{a}");
        assert!(a.contains("FICA"), "{a}");
        // Descartável não precisa alertar nada — seria só custo de contexto.
        s.mount = Mount::Overlay;
        assert!(alerta_de_escrita_real(&s).is_none());
        // Nem faz sentido alertar quando não há pasta montada.
        s.mount = Mount::ReadWrite;
        s.workdir = None;
        assert!(alerta_de_escrita_real(&s).is_none());
    }

    #[test]
    fn the_listing_shows_folder_and_mode() {
        use orchestrator_sandbox::container::{Aberta, Mount};
        let texto = lista_abertas(&[
            Aberta {
                name: "conferencia".into(),
                since: "2 minutes".into(),
                workdir: Some(PathBuf::from("/repo")),
                mount: Some(Mount::ReadWrite),
            },
            Aberta {
                name: "solta".into(),
                since: "5 seconds".into(),
                workdir: None,
                mount: None,
            },
        ]);
        assert!(
            texto.contains("- conferencia (de pé há 2 minutes, /work = /repo, escrita real"),
            "{texto}"
        );
        assert!(texto.contains("- solta (de pé há 5 seconds)"), "{texto}");
    }

    #[test]
    fn stopping_a_name_that_never_existed_does_not_claim_it_was_closed() {
        // "já estava encerrada" num nome errado fazia ele marcar a limpeza
        // como feita enquanto a sandbox de verdade seguia de pé.
        let t = call("ui_stop", &json!({ "name": "nome-que-nunca-existiu-xyz" })).unwrap();
        assert!(t.contains("não há sandbox"), "{t}");
        assert!(!t.contains("encerrada\""), "{t}");
    }

    #[test]
    fn exec_warns_that_the_command_runs_in_the_foreground() {
        let d = tool_descriptors()
            .into_iter()
            .find(|t| t["name"] == "ui_exec")
            .unwrap();
        let desc = d["description"].as_str().unwrap();
        // Sem isto o modelo subia um servidor em primeiro plano e prendia o
        // processo do MCP inteiro, inclusive o ui_stop.
        assert!(desc.contains("PRIMEIRO PLANO"), "{desc}");
        assert!(desc.contains("setsid"), "{desc}");
        assert!(desc.contains("120s"), "{desc}");
    }

    #[test]
    fn unknown_ui_tool_is_rejected() {
        assert!(call("ui_voar", &json!({})).is_err());
    }
}
