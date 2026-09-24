//! Acesso remoto: uma página de controle do Orchestrator servida só no
//! loopback (`127.0.0.1:9998`), atrás de senha validada NO SERVIDOR, para ser
//! alcançada de fora por um túnel Cloudflare que o dono liga/desliga.
//!
//! Regras que não se quebram:
//! - **bind só em `127.0.0.1`**, nunca `0.0.0.0`: nada fica exposto por IP+porta
//!   na rede local; a única entrada é o túnel, e o túnel exige a senha.
//! - **senha no servidor**, conferida a cada acesso; o cliente nunca decide.
//!   A senha vira `sha256` iterado com sal (nunca guardamos o texto).
//! - nada aqui disfarça tráfego: é HTTPS de verdade (o Cloudflare termina o
//!   TLS) e streaming por SSE, que é HTTP puro — sem WebSocket.

use std::collections::HashSet;
use std::io::{BufRead, Read};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::extract::{Path, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{json, Value};
use tauri::{AppHandle, Manager};
use tokio_stream::wrappers::IntervalStream;
use tokio_stream::StreamExt;

use orchestrator_engine::workbench::Engine;
use orchestrator_memory::store::MemoryStore;

/// O Engine compartilhado (o MESMO da janela). O servidor web nunca passa
/// pelo `State` do Tauri — segura um clone deste Arc.
type Motor = Arc<Mutex<Engine>>;

/// A porta do servidor local (pedido do dono: incomum). Só no loopback.
pub const PORTA: u16 = 9998;

const SENHA_HASH: &str = "remoto.senha.hash";
const SENHA_SALT: &str = "remoto.senha.salt";
const ITERACOES: usize = 100_000;

// ---------------------------------------------------------------- senha

fn urandom(n: usize) -> Vec<u8> {
    let mut b = vec![0u8; n];
    let _ = std::fs::File::open("/dev/urandom").and_then(|mut f| f.read_exact(&mut b));
    b
}

fn hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

fn hash_senha(salt: &[u8], senha: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut atual = {
        let mut h = Sha256::new();
        h.update(salt);
        h.update(senha.as_bytes());
        h.finalize().to_vec()
    };
    for _ in 0..ITERACOES {
        let mut h = Sha256::new();
        h.update(&atual);
        atual = h.finalize().to_vec();
    }
    hex(&atual)
}

/// Compara sem vazar tempo (não sai no primeiro byte diferente).
fn igual_ct(a: &str, b: &str) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.bytes().zip(b.bytes()).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

pub fn definir_senha(store: &MemoryStore, senha: &str) -> Result<(), String> {
    if senha.chars().count() < 6 {
        return Err("a senha precisa de pelo menos 6 caracteres".into());
    }
    let salt = urandom(16);
    let _ = store.ui_set(SENHA_SALT, &hex(&salt));
    store.ui_set(SENHA_HASH, &hash_senha(&salt, senha)).map_err(|e| e.to_string())
}

pub fn senha_definida(store: &MemoryStore) -> bool {
    matches!(store.ui_get(SENHA_HASH), Ok(Some(h)) if !h.is_empty())
}

fn senha_confere(store: &MemoryStore, senha: &str) -> bool {
    let (Ok(Some(salt_hex)), Ok(Some(hash))) = (store.ui_get(SENHA_SALT), store.ui_get(SENHA_HASH)) else {
        return false;
    };
    let salt: Vec<u8> = (0..salt_hex.len() / 2)
        .filter_map(|i| u8::from_str_radix(&salt_hex[i * 2..i * 2 + 2], 16).ok())
        .collect();
    igual_ct(&hash_senha(&salt, senha), &hash)
}

// ---------------------------------------------------------------- estado do servidor

type Sessoes = Arc<Mutex<HashSet<String>>>;

#[derive(Clone)]
struct Estado {
    motor: Motor,
    sessoes: Sessoes,
}

fn token_do_cookie(headers: &HeaderMap) -> Option<String> {
    headers
        .get(header::COOKIE)?
        .to_str()
        .ok()?
        .split(';')
        .find_map(|p| p.trim().strip_prefix("sess=").map(str::to_string))
}

fn autenticado(e: &Estado, headers: &HeaderMap) -> bool {
    match token_do_cookie(headers) {
        Some(t) => e.sessoes.lock().map(|s| s.contains(&t)).unwrap_or(false),
        None => false,
    }
}

fn negar() -> Response {
    (StatusCode::UNAUTHORIZED, "faça login").into_response()
}

/// Bloqueia até o Engine e devolve a `Foto` serializada (o mesmo estado que a
/// janela desenha).
fn foto_json(motor: &Motor) -> String {
    match motor.lock() {
        Ok(e) => serde_json::to_string(&crate::foto(&e)).unwrap_or_else(|_| "{}".into()),
        Err(_) => "{}".into(),
    }
}

// ---------------------------------------------------------------- rotas

async fn raiz(State(e): State<Estado>, headers: HeaderMap) -> Response {
    if autenticado(&e, &headers) {
        Html(PAGINA_APP).into_response()
    } else {
        Html(PAGINA_LOGIN).into_response()
    }
}

#[derive(Deserialize)]
struct Login {
    senha: String,
}

async fn login(State(e): State<Estado>, Json(corpo): Json<Login>) -> Response {
    let ok = match e.motor.lock() {
        Ok(eng) => senha_confere(&eng.store, &corpo.senha),
        Err(_) => false,
    };
    if !ok {
        // Um passo de atraso desestimula tentativa em massa.
        std::thread::sleep(Duration::from_millis(400));
        return (StatusCode::UNAUTHORIZED, "senha incorreta").into_response();
    }
    let token = hex(&urandom(24));
    if let Ok(mut s) = e.sessoes.lock() {
        s.insert(token.clone());
    }
    let cookie = format!("sess={token}; HttpOnly; SameSite=Strict; Secure; Path=/; Max-Age=86400");
    ([(header::SET_COOKIE, cookie)], Json(json!({ "ok": true }))).into_response()
}

async fn logout(State(e): State<Estado>, headers: HeaderMap) -> Response {
    if let Some(t) = token_do_cookie(&headers) {
        if let Ok(mut s) = e.sessoes.lock() {
            s.remove(&t);
        }
    }
    let limpar = "sess=; HttpOnly; SameSite=Strict; Secure; Path=/; Max-Age=0";
    ([(header::SET_COOKIE, limpar)], Json(json!({ "ok": true }))).into_response()
}

async fn estado_sse(State(e): State<Estado>, headers: HeaderMap) -> Response {
    if !autenticado(&e, &headers) {
        return negar();
    }
    let motor = e.motor.clone();
    let stream = IntervalStream::new(tokio::time::interval(Duration::from_millis(500))).map(move |_| {
        Ok::<Event, std::convert::Infallible>(Event::default().data(foto_json(&motor)))
    });
    Sse::new(stream).keep_alive(KeepAlive::default()).into_response()
}

#[derive(Deserialize)]
struct Enviar {
    texto: String,
}

async fn enviar(State(e): State<Estado>, headers: HeaderMap, Json(corpo): Json<Enviar>) -> Response {
    if !autenticado(&e, &headers) {
        return negar();
    }
    use orchestrator_engine::workbench::CommandOutcome;
    let Ok(mut eng) = e.motor.lock() else {
        return (StatusCode::INTERNAL_SERVER_ERROR, "núcleo travado").into_response();
    };
    if eng.run_command(&corpo.texto) == CommandOutcome::NotCommand {
        eng.chat.input = corpo.texto;
        eng.send_chat(None);
    }
    // Os `EngineEvent` (focar grade/chat, abrir manual) são dicas de foco da
    // janela — não fazem sentido no controle remoto, então só descartamos.
    let _ = eng.take_events();
    Json(json!({ "ok": true })).into_response()
}

async fn acao(State(e): State<Estado>, Path(cmd): Path<String>, headers: HeaderMap, Json(a): Json<Value>) -> Response {
    if !autenticado(&e, &headers) {
        return negar();
    }
    let Ok(mut eng) = e.motor.lock() else {
        return (StatusCode::INTERNAL_SERVER_ERROR, "núcleo travado").into_response();
    };
    let s = |k: &str| a.get(k).and_then(Value::as_str).unwrap_or("").to_string();
    let i = |k: &str| a.get(k).and_then(Value::as_u64).unwrap_or(0) as usize;
    match cmd.as_str() {
        "resolver_decisao" => eng.resolve_by_chat(a.get("aprovar").and_then(Value::as_bool).unwrap_or(false), Some(&s("id"))),
        "responder_pergunta" => {
            if let Err(err) = eng.answer_question(&s("id"), &s("resposta")) {
                return (StatusCode::BAD_REQUEST, err).into_response();
            }
        }
        "trocar_workspace" => eng.switch_workspace(i("indice")),
        "focar_card" => {
            let ws = eng.ws_idx;
            if i("indice") < eng.workspaces[ws].panes.len() {
                eng.workspaces[ws].focused = i("indice");
            }
        }
        outro => return (StatusCode::BAD_REQUEST, format!("ação desconhecida: {outro}")).into_response(),
    }
    Json(json!({ "ok": true })).into_response()
}

fn rotas(estado: Estado) -> Router {
    Router::new()
        .route("/", get(raiz))
        .route("/login", post(login))
        .route("/logout", post(logout))
        .route("/estado", get(estado_sse))
        .route("/enviar", post(enviar))
        .route("/acao/{cmd}", post(acao))
        .with_state(estado)
}

/// Sobe o servidor (uma vez). Bind SÓ no loopback — nunca 0.0.0.0.
pub async fn servir(motor: Motor, sessoes: Sessoes) -> anyhow::Result<()> {
    let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, PORTA)).await?;
    axum::serve(listener, rotas(Estado { motor, sessoes })).await?;
    Ok(())
}

// ---------------------------------------------------------------- túnel

/// Estado do acesso remoto, guardado pelo app (comando Tauri).
#[derive(Default)]
pub struct Remoto {
    servidor_no_ar: AtomicBool,
    sessoes: Mutex<Option<Sessoes>>,
    tunel: Mutex<Option<Tunel>>,
}

struct Tunel {
    filho: std::process::Child,
    url: String,
}

impl Remoto {
    fn sessoes(&self) -> Sessoes {
        let mut g = self.sessoes.lock().expect("sessoes");
        g.get_or_insert_with(|| Arc::new(Mutex::new(HashSet::new()))).clone()
    }

    /// Garante o servidor local no ar (idempotente).
    fn garantir_servidor(&self, motor: Motor) {
        if self.servidor_no_ar.swap(true, Ordering::SeqCst) {
            return;
        }
        let sessoes = self.sessoes();
        tauri::async_runtime::spawn(async move {
            if let Err(e) = servir(motor, sessoes).await {
                eprintln!("servidor remoto caiu: {e:#}");
            }
        });
    }

    pub fn url(&self) -> Option<String> {
        self.tunel.lock().ok()?.as_ref().map(|t| t.url.clone())
    }

    /// Liga o túnel Cloudflare apontando para o servidor local. Exige a senha
    /// já definida. Devolve a URL pública.
    pub fn ligar(&self, app: &AppHandle) -> Result<String, String> {
        let motor: Motor = app.state::<crate::Nucleo>().0.clone();
        {
            let eng = motor.lock().map_err(|_| "núcleo travado".to_string())?;
            if !senha_definida(&eng.store) {
                return Err("defina uma senha antes de abrir o túnel".into());
            }
        }
        if let Some(t) = self.tunel.lock().map_err(|_| "estado travado".to_string())?.as_ref() {
            return Ok(t.url.clone());
        }
        self.garantir_servidor(motor);
        std::thread::sleep(Duration::from_millis(300));

        let mut filho = std::process::Command::new("cloudflared")
            .args(["tunnel", "--no-autoupdate", "--url", &format!("http://127.0.0.1:{PORTA}")])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .map_err(|e| format!("não consegui rodar o cloudflared: {e} (instale-o primeiro)"))?;

        // O cloudflared imprime a URL no stderr. Lê até achar, depois deixa uma
        // thread drenando o resto (senão o buffer do pipe enche e ele trava).
        let stderr = filho.stderr.take().ok_or("cloudflared sem stderr")?;
        let (tx, rx) = std::sync::mpsc::channel::<String>();
        std::thread::spawn(move || {
            let mut leitor = std::io::BufReader::new(stderr);
            let mut linha = String::new();
            let mut achou = false;
            loop {
                linha.clear();
                match leitor.read_line(&mut linha) {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {
                        if !achou {
                            if let Some(i) = linha.find("https://") {
                                let url: String = linha[i..]
                                    .chars()
                                    .take_while(|c| !c.is_whitespace())
                                    .collect();
                                if url.contains("trycloudflare.com") {
                                    achou = true;
                                    let _ = tx.send(url.trim_end_matches(['|', ' ']).to_string());
                                }
                            }
                        }
                    }
                }
            }
        });

        match rx.recv_timeout(Duration::from_secs(25)) {
            Ok(url) => {
                *self.tunel.lock().map_err(|_| "estado travado".to_string())? = Some(Tunel { filho, url: url.clone() });
                Ok(url)
            }
            Err(_) => {
                let _ = filho.kill();
                Err("o cloudflared não devolveu uma URL em 25s — tente de novo".into())
            }
        }
    }

    pub fn desligar(&self) -> Result<(), String> {
        if let Some(mut t) = self.tunel.lock().map_err(|_| "estado travado".to_string())?.take() {
            let _ = t.filho.kill();
            let _ = t.filho.wait();
        }
        Ok(())
    }
}

// ---------------------------------------------------------------- páginas

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt;

    fn motor_teste() -> Motor {
        let cfg = orchestrator_core::Config::default();
        let store = MemoryStore::open_in_memory().unwrap();
        let eng = Engine::new(store, &cfg, std::path::PathBuf::from("mem.db"));
        Arc::new(Mutex::new(eng))
    }

    #[test]
    fn hash_confere_e_rejeita_senha_curta() {
        let store = MemoryStore::open_in_memory().unwrap();
        assert!(!senha_definida(&store));
        assert!(definir_senha(&store, "123").is_err());
        definir_senha(&store, "segredo").unwrap();
        assert!(senha_definida(&store));
        assert!(senha_confere(&store, "segredo"));
        assert!(!senha_confere(&store, "Segredo"));
    }

    #[tokio::test]
    async fn a_senha_e_conferida_no_servidor_e_a_sessao_libera() {
        let motor = motor_teste();
        {
            let e = motor.lock().unwrap();
            definir_senha(&e.store, "segredo123").unwrap();
        }
        let sessoes: Sessoes = Arc::new(Mutex::new(HashSet::new()));
        let rotas = || super::rotas(Estado { motor: motor.clone(), sessoes: sessoes.clone() });

        // Sem sessão, o estado é negado.
        let r = rotas().oneshot(Request::builder().uri("/estado").body(Body::empty()).unwrap()).await.unwrap();
        assert_eq!(r.status(), StatusCode::UNAUTHORIZED);

        // Senha errada não entra.
        let r = rotas()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/login")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"senha":"errada"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(r.status(), StatusCode::UNAUTHORIZED);

        // Senha certa devolve o cookie de sessão.
        let r = rotas()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/login")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"senha":"segredo123"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(r.status(), StatusCode::OK);
        let sc = r.headers().get(header::SET_COOKIE).unwrap().to_str().unwrap();
        assert!(sc.contains("sess=") && sc.contains("HttpOnly"));
        let cookie = sc.split(';').next().unwrap().to_string();

        // Com a sessão, o estado abre.
        let r = rotas()
            .oneshot(Request::builder().uri("/estado").header(header::COOKIE, &cookie).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(r.status(), StatusCode::OK);
    }
}

const PAGINA_LOGIN: &str = r#"<!doctype html><html lang="pt-br"><head><meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1"><title>Orchestrator — entrar</title>
<style>
:root{color-scheme:dark}body{margin:0;height:100vh;display:grid;place-items:center;background:#0d1017;color:#e6e6e6;font:15px/1.5 system-ui,sans-serif}
form{width:min(340px,90vw);background:#151a23;border:1px solid #232a36;border-radius:14px;padding:26px}
h1{margin:0 0 4px;font-size:18px}p{margin:0 0 18px;color:#8b95a5;font-size:13px}
input{width:100%;box-sizing:border-box;height:42px;padding:0 12px;border:1px solid #2a3342;border-radius:9px;background:#0d1017;color:#e6e6e6;font-size:15px;outline:none}
input:focus{border-color:#4f8cff}button{margin-top:12px;width:100%;height:42px;border:0;border-radius:9px;background:#4f8cff;color:#fff;font-size:15px;font-weight:600;cursor:pointer}
.erro{color:#ff6b6b;font-size:13px;margin-top:10px;min-height:16px}
</style></head><body><form id="f">
<h1>Orchestrator</h1><p>Acesso remoto protegido. Digite a senha.</p>
<input id="s" type="password" placeholder="senha" autocomplete="current-password" autofocus>
<button>Entrar</button><div class="erro" id="e"></div>
</form><script>
const f=document.getElementById('f'),s=document.getElementById('s'),e=document.getElementById('e');
f.onsubmit=async ev=>{ev.preventDefault();e.textContent='';
 const r=await fetch('/login',{method:'POST',headers:{'content-type':'application/json'},body:JSON.stringify({senha:s.value})});
 if(r.ok){location.href='/'}else{e.textContent='Senha incorreta.';s.value='';s.focus()}};
</script></body></html>"#;

const PAGINA_APP: &str = r#"<!doctype html><html lang="pt-br"><head><meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1"><title>Orchestrator</title>
<style>
:root{color-scheme:dark}*{box-sizing:border-box}body{margin:0;background:#0d1017;color:#e6e6e6;font:14px/1.5 system-ui,sans-serif;display:flex;flex-direction:column;height:100vh}
header{display:flex;align-items:center;gap:10px;padding:10px 14px;border-bottom:1px solid #232a36;background:#151a23;flex-wrap:wrap}
header b{font-size:15px}.chip{font-size:12px;color:#8b95a5;background:#0d1017;border:1px solid #2a3342;border-radius:20px;padding:2px 10px}
header .sair{margin-left:auto;font-size:12px;color:#8b95a5;background:none;border:0;cursor:pointer}
main{flex:1;overflow-y:auto;padding:12px 14px;display:flex;flex-direction:column;gap:10px}
.linha{max-width:90%}.linha .quem{font-size:11px;color:#8b95a5}.linha .txt{white-space:pre-wrap;word-break:break-word}
.linha.voce{align-self:flex-end;text-align:right}
.sec{margin-top:6px}.sec h2{font-size:12px;text-transform:uppercase;letter-spacing:.04em;color:#8b95a5;margin:0 0 6px}
.card{border:1px solid #232a36;border-radius:9px;padding:8px 10px;margin-bottom:6px;background:#151a23}
.card .top{display:flex;gap:8px;align-items:center;font-size:13px}.card .st{font-size:11px;color:#8b95a5}
.card pre{margin:6px 0 0;max-height:160px;overflow:auto;font:12px/1.4 ui-monospace,monospace;color:#c9d3e0;white-space:pre-wrap}
.dec{border:1px solid #3a2a2a;background:#1a1414;border-radius:9px;padding:10px;margin-bottom:8px}
.dec .btns{display:flex;gap:8px;margin-top:8px}.dec button{flex:1;height:34px;border:0;border-radius:8px;cursor:pointer;font-weight:600}
.sim{background:#2e7d32;color:#fff}.nao{background:#5a2a2a;color:#fff}
footer{display:flex;gap:8px;padding:10px 14px;border-top:1px solid #232a36;background:#151a23}
footer input{flex:1;height:40px;padding:0 12px;border:1px solid #2a3342;border-radius:9px;background:#0d1017;color:#e6e6e6;font-size:15px;outline:none}
footer button{height:40px;padding:0 16px;border:0;border-radius:9px;background:#4f8cff;color:#fff;font-weight:600;cursor:pointer}
.ws{display:flex;gap:6px;flex-wrap:wrap;margin-bottom:6px}.ws button{font-size:12px;padding:4px 10px;border:1px solid #2a3342;border-radius:20px;background:#0d1017;color:#8b95a5;cursor:pointer}
.ws button.on{border-color:#4f8cff;color:#e6e6e6}
</style></head><body>
<header><b>Orchestrator</b><span class="chip" id="proj">—</span><span class="chip" id="prov">—</span><span class="chip" id="post">—</span><button class="sair" onclick="sair()">sair</button></header>
<main id="main"></main>
<footer><input id="in" placeholder="mensagem ou /comando" autocomplete="off"><button onclick="enviar()">Enviar</button></footer>
<script>
let ultima=0;
const el=id=>document.getElementById(id);
function esc(t){const d=document.createElement('div');d.textContent=t==null?'':String(t);return d.innerHTML}
async function post(u,b){return fetch(u,{method:'POST',headers:{'content-type':'application/json'},body:JSON.stringify(b||{})})}
function enviar(){const i=el('in');const t=i.value.trim();if(!t)return;i.value='';post('/enviar',{texto:t})}
el('in').addEventListener('keydown',e=>{if(e.key==='Enter')enviar()});
function sair(){post('/logout').then(()=>location.href='/')}
function acao(c,a){post('/acao/'+c,a)}
function render(f){
 el('proj').textContent=f.projeto||'—';el('prov').textContent=(f.provedor||'')+(f.modelo?(' · '+f.modelo):'');el('post').textContent=f.postura||'';
 const m=el('main');const perto=m.scrollTop+m.clientHeight>m.scrollHeight-60;let h='';
 const dec=(f.decisoes||[]).filter(d=>d.revisor==='dono');
 if(dec.length){h+='<div class="sec"><h2>Precisa de você</h2>';for(const d of dec){
   h+='<div class="dec"><div>'+esc(d.resumo)+'</div>';
   if(d.tipo==='pergunta'&&d.opcoes&&d.opcoes.length){h+='<div class="btns">';for(const o of d.opcoes){h+='<button class="sim" onclick=\'acao("responder_pergunta",{id:"'+d.id+'",resposta:'+JSON.stringify(o)+'})\'>'+esc(o)+'</button>'}h+='</div>';}
   else{h+='<div class="btns"><button class="sim" onclick=\'acao("resolver_decisao",{id:"'+d.id+'",aprovar:true})\'>Aprovar</button><button class="nao" onclick=\'acao("resolver_decisao",{id:"'+d.id+'",aprovar:false})\'>Negar</button></div>';}
   h+='</div>';}h+='</div>';}
 h+='<div class="sec"><h2>Conversa</h2>';
 for(const l of (f.chat||[])){const v=(l.quem==='você'||l.quem==='voce');h+='<div class="linha '+(v?'voce':'')+'"><div class="quem">'+esc(l.quem)+'</div><div class="txt">'+esc(l.texto)+'</div></div>';}
 h+='</div>';
 const wss=f.workspaces||[];if(wss.length){h+='<div class="sec"><h2>Workspaces</h2><div class="ws">';
  wss.forEach((w,idx)=>{h+='<button class="'+(idx===f.workspace?'on':'')+'" onclick=\'acao("trocar_workspace",{indice:'+idx+'})\'>'+(idx+1)+(w.cards&&w.cards.length?(' · '+w.cards.length):'')+'</button>'});h+='</div>';
  const ws=wss[f.workspace];if(ws){for(const c of (ws.cards||[])){h+='<div class="card"><div class="top">'+esc(c.nome)+' <span class="st">'+esc(c.estado)+(c.detalhe?(' · '+esc(c.detalhe)):'')+'</span></div>'+(c.saida?('<pre>'+esc(c.saida)+'</pre>'):'')+'</div>';}}
  h+='</div>';}
 m.innerHTML=h;if(perto)m.scrollTop=m.scrollHeight;
}
function conectar(){const es=new EventSource('/estado');
 es.onmessage=e=>{try{render(JSON.parse(e.data))}catch(_){}};
 es.onerror=()=>{es.close();setTimeout(()=>{fetch('/estado').then(r=>{if(r.status===401)location.href='/';else conectar()}).catch(()=>setTimeout(conectar,2000))},1500)};
}
conectar();
</script></body></html>"#;
