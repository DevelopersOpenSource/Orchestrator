//! A memória como ferramenta: API HTTP e a página, só no loopback.
//!
//! - **leitura livre** para quem está nesta máquina;
//! - **escrita** (criar, editar, apagar) só com `Authorization: Bearer <token>`,
//!   token em `~/.config/orchestrator/memory-api.token` (0600, criado na 1ª vez);
//! - o `Host` precisa ser `127.0.0.1` ou `localhost` na nossa porta: contra DNS
//!   rebinding, uma página da internet aberta neste navegador não fala com a API;
//! - CORS só para a própria página e o app desktop;
//! - as regras do banco valem aqui também: memória de IA não se edita.

use std::net::Ipv4Addr;
use std::path::{Path as FsPath, PathBuf};
use std::str::FromStr;
use std::sync::Arc;
use std::time::Instant;

use anyhow::{bail, Context, Result};
use axum::extract::{Path, Query, Request, State};
use axum::http::{header, HeaderMap, HeaderValue, Method, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use rust_embed::Embed;
use serde::Deserialize;
use serde_json::{json, Value};
use tower_http::cors::{AllowOrigin, CorsLayer};

use orchestrator_memory::daemon;
use orchestrator_memory::store::NewMemory;
use orchestrator_memory::{Memory, MemoryKind, Origin, GLOBAL_PROJECT};

use crate::engine::Engine;

/// Porta configurável.
pub const PORT_ENV: &str = "ORCHESTRATOR_MEMORY_PORT";
/// Porta padrão; ocupada, tenta as seguintes.
pub const DEFAULT_PORT: u16 = 10000;
const PORT_TRIES: u16 = 20;
const MAX_TITLE_CHARS: usize = 200;
const MAX_BODY_CHARS: usize = 20_000;

#[derive(Clone)]
struct Api {
    engine: Arc<Engine>,
    port: u16,
    token: Arc<str>,
}

/// Onde o token de escrita fica (ao lado do config.json).
pub fn token_path() -> PathBuf {
    orchestrator_core::config::default_config_path()
        .ok()
        .and_then(|p| p.parent().map(FsPath::to_path_buf))
        .unwrap_or_else(|| std::env::temp_dir().join("orchestrator"))
        .join("memory-api.token")
}

/// Arquivo com a porta em uso (para a CLI, o app e quem mais precisar).
pub fn port_file() -> PathBuf {
    daemon::socket_path().with_file_name("memory-api.port")
}

/// Lê o token, ou cria um novo (64 hex de UUIDs v4 — gerador do sistema) só
/// legível pelo dono.
pub fn load_or_create_token(path: &FsPath) -> Result<String> {
    if let Ok(t) = std::fs::read_to_string(path) {
        let t = t.trim();
        if t.len() >= 32 {
            return Ok(t.to_string());
        }
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("criando {}", dir.display()))?;
    }
    let token = format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    );
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    use std::io::Write;
    let mut f = opts
        .open(path)
        .with_context(|| format!("gravando {}", path.display()))?;
    f.write_all(token.as_bytes())?;
    #[cfg(unix)]
    {
        // Um arquivo que já existia pode ter vindo com permissão larga.
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(token)
}

/// Abre a primeira porta livre a partir da configurada, só no loopback.
pub async fn bind() -> Result<(tokio::net::TcpListener, u16)> {
    let base = std::env::var(PORT_ENV)
        .ok()
        .and_then(|v| v.trim().parse::<u16>().ok())
        .unwrap_or(DEFAULT_PORT);
    for porta in base..base.saturating_add(PORT_TRIES) {
        if let Ok(l) = tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, porta)).await {
            let _ = std::fs::write(port_file(), porta.to_string());
            return Ok((l, porta));
        }
    }
    bail!(
        "nenhuma porta livre entre {base} e {} para a API da memória",
        base.saturating_add(PORT_TRIES - 1)
    )
}

/// Serve a API até o processo acabar.
pub async fn serve(
    engine: Arc<Engine>,
    listener: tokio::net::TcpListener,
    port: u16,
    token: String,
) -> Result<()> {
    axum::serve(listener, router(engine, port, token))
        .await
        .context("servindo a API da memória")
}

/// As rotas, com as proteções.
pub fn router(engine: Arc<Engine>, port: u16, token: String) -> Router {
    let api = Api {
        engine,
        port,
        token: Arc::from(token),
    };
    let origens: Vec<HeaderValue> = [
        format!("http://127.0.0.1:{port}"),
        format!("http://localhost:{port}"),
        // App desktop (Tauri) no Linux/macOS e no Windows.
        "tauri://localhost".to_string(),
        "http://tauri.localhost".to_string(),
        // `npm run tauri dev` abre a janela pelo servidor do Vite.
        #[cfg(debug_assertions)]
        "http://localhost:5179".to_string(),
    ]
    .iter()
    .filter_map(|o| HeaderValue::from_str(o).ok())
    .collect();
    let cors = CorsLayer::new()
        .allow_origin(AllowOrigin::list(origens))
        .allow_methods([Method::GET, Method::POST, Method::PUT, Method::DELETE])
        .allow_headers([header::AUTHORIZATION, header::CONTENT_TYPE]);

    Router::new()
        .route("/", get(indice))
        .route("/assets/{*path}", get(asset))
        .route("/api/health", get(health))
        .route("/api/search", get(search))
        .route("/api/memories", get(list).post(create))
        .route("/api/memories/{id}", get(one).put(update).delete(remove))
        .route("/api/graph", get(graph))
        .route("/api/context", get(context))
        .layer(middleware::from_fn_with_state(api.clone(), guarda))
        // CORS por fora: até a recusa chega legível à própria página.
        .layer(cors)
        .with_state(api)
}

fn erro(status: StatusCode, msg: impl Into<String>) -> Response {
    (status, Json(json!({ "ok": false, "error": msg.into() }))).into_response()
}

/// `Host` precisa ser o nosso loopback; escrita precisa do token.
async fn guarda(State(api): State<Api>, req: Request, next: Next) -> Response {
    let host = req
        .headers()
        .get(header::HOST)
        .and_then(|h| h.to_str().ok())
        .unwrap_or("");
    if !host_permitido(host, api.port) {
        return erro(
            StatusCode::MISDIRECTED_REQUEST,
            "host não permitido: a API da memória só atende 127.0.0.1 e localhost",
        );
    }
    let escrita = matches!(
        *req.method(),
        Method::POST | Method::PUT | Method::DELETE | Method::PATCH
    );
    if escrita && !token_confere(&api, req.headers()) {
        return erro(
            StatusCode::UNAUTHORIZED,
            format!(
                "escrita exige Authorization: Bearer <token> — o token fica em {}",
                token_path().display()
            ),
        );
    }
    api.engine.touch();
    next.run(req).await
}

/// O `Host` é o nosso endereço de loopback?
pub fn host_permitido(host: &str, port: u16) -> bool {
    host == format!("127.0.0.1:{port}") || host == format!("localhost:{port}")
}

fn token_confere(api: &Api, headers: &HeaderMap) -> bool {
    let dado = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .unwrap_or("");
    iguais(dado.as_bytes(), api.token.as_bytes())
}

/// Comparação sem atalho (não revela quantos caracteres acertou).
fn iguais(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

fn memoria_json(m: &Memory) -> Value {
    json!({
        "id": m.id, "project": m.project, "kind": m.kind.as_str(), "scope": m.scope.as_str(),
        "origin": m.origin.as_str(), "author": m.author, "title": m.title, "body": m.body,
        "priority": m.priority, "created_at": m.created_at, "updated_at": m.updated_at,
    })
}

fn tipo(kind: Option<&str>) -> Result<Option<MemoryKind>, String> {
    match kind.filter(|k| !k.is_empty()) {
        None => Ok(None),
        Some(k) => MemoryKind::from_str(k).map(Some).map_err(|e| e.to_string()),
    }
}

fn projeto(p: Option<&str>) -> Option<&str> {
    p.map(str::trim).filter(|p| !p.is_empty())
}

fn validar(title: &str, body: &str, priority: i64) -> Result<(), String> {
    if title.trim().is_empty() || title.chars().count() > MAX_TITLE_CHARS {
        return Err(format!("title precisa ter de 1 a {MAX_TITLE_CHARS} caracteres"));
    }
    if body.chars().count() > MAX_BODY_CHARS {
        return Err(format!("body passa de {MAX_BODY_CHARS} caracteres"));
    }
    if !(0..=10).contains(&priority) {
        return Err("priority vai de 0 a 10".into());
    }
    Ok(())
}

fn agendar_indice(api: &Api) {
    let engine = api.engine.clone();
    tokio::spawn(async move {
        if let Err(e) = engine.index_pending().await {
            tracing::warn!("indexação depois de escrita pela API falhou: {e:#}");
        }
    });
}

/// A página do globo, gerada por `web/apps/memory` (`npm run build:memory`) e
/// embutida no binário. Sem o build, a rota `/` mostra a página provisória.
#[derive(Embed)]
#[folder = "../../web/apps/memory/dist/"]
#[allow_missing = true]
struct Pagina;

/// Segurança da página: nada de fora, nada de script inline. Uma memória com
/// HTML dentro nunca vira código executado.
const CSP: &str = "default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; \
    img-src 'self' data: blob:; font-src 'self'; connect-src 'self'; object-src 'none'; \
    base-uri 'none'; frame-ancestors 'none'; form-action 'self'";

async fn indice(State(api): State<Api>) -> Response {
    match Pagina::get("index.html") {
        Some(arquivo) => (
            [
                (header::CONTENT_TYPE, "text/html; charset=utf-8".to_string()),
                (header::CACHE_CONTROL, "no-cache".to_string()),
                (header::CONTENT_SECURITY_POLICY, CSP.to_string()),
                (header::X_CONTENT_TYPE_OPTIONS, "nosniff".to_string()),
                (header::REFERRER_POLICY, "no-referrer".to_string()),
            ],
            arquivo.data.into_owned(),
        )
            .into_response(),
        None => pagina_provisoria(api.port).into_response(),
    }
}

async fn asset(Path(path): Path<String>) -> Response {
    match Pagina::get(&format!("assets/{path}")) {
        Some(arquivo) => {
            let tipo = mime_guess::from_path(&path).first_or_octet_stream();
            (
                [
                    (header::CONTENT_TYPE, tipo.to_string()),
                    // Os nomes têm hash do conteúdo: podem ficar em cache.
                    (header::CACHE_CONTROL, "public, max-age=31536000, immutable".to_string()),
                    (header::X_CONTENT_TYPE_OPTIONS, "nosniff".to_string()),
                ],
                arquivo.data.into_owned(),
            )
                .into_response()
        }
        None => erro(StatusCode::NOT_FOUND, "arquivo não encontrado"),
    }
}

fn pagina_provisoria(porta: u16) -> Html<String> {
    Html(format!(
        "<!doctype html><html lang=\"pt-BR\"><meta charset=\"utf-8\"><title>Memória · Orchestrator</title>\
         <body style=\"font-family:system-ui,sans-serif;max-width:40rem;margin:3rem auto;padding:0 1rem;line-height:1.5\">\
         <h1>Memória do Orchestrator</h1><p>A API está no ar na porta {porta}. A página com o globo chega na próxima etapa.</p>\
         <ul><li><code>GET /api/health</code></li><li><code>GET /api/search?q=…&amp;project=…&amp;rerank=true</code></li>\
         <li><code>GET /api/memories?project=…</code> · <code>GET /api/memories/{{id}}</code></li>\
         <li><code>GET /api/graph?project=…</code> · <code>GET /api/context?project=…&amp;prompt=…</code></li>\
         <li><code>POST/PUT/DELETE /api/memories</code> com <code>Authorization: Bearer &lt;token&gt;</code></li></ul></body></html>",
        porta = porta
    ))
}

async fn health(State(api): State<Api>) -> Json<Value> {
    let e = &api.engine;
    let (modelos, chroma) = (e.models_ready(), e.has_collection().await);
    Json(json!({
        "ok": true,
        "models": e.models_state(),
        "models_ready": modelos,
        "chroma": chroma,
        "semantic": modelos && chroma,
        "memories": e.store().all_ids().map(|v| v.len()).unwrap_or(0),
        "port": api.port,
        "version": env!("CARGO_PKG_VERSION"),
    }))
}

#[derive(Deserialize)]
struct Busca {
    q: String,
    project: Option<String>,
    kind: Option<String>,
    limit: Option<usize>,
    rerank: Option<bool>,
}

async fn search(State(api): State<Api>, Query(b): Query<Busca>) -> Response {
    let q = b.q.trim();
    if q.is_empty() {
        return erro(StatusCode::BAD_REQUEST, "informe q (o que procurar)");
    }
    let kind = match tipo(b.kind.as_deref()) {
        Ok(k) => k,
        Err(msg) => return erro(StatusCode::BAD_REQUEST, msg),
    };
    let inicio = Instant::now();
    match api
        .engine
        .search(projeto(b.project.as_deref()), q, b.limit.unwrap_or(10), kind, b.rerank.unwrap_or(true))
        .await
    {
        Ok(r) => Json(json!({
            "ok": true,
            "query": q,
            "took_ms": inicio.elapsed().as_millis() as u64,
            "semantic": r.semantic,
            "reranked": r.reranked,
            "hits": r.hits.iter().map(|s| {
                let mut v = memoria_json(&s.memory);
                v["score"] = json!(s.score);
                v
            }).collect::<Vec<_>>(),
        }))
        .into_response(),
        Err(e) => erro(StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")),
    }
}

#[derive(Deserialize)]
struct Lista {
    project: Option<String>,
    scope: Option<String>,
    kind: Option<String>,
}

async fn list(State(api): State<Api>, Query(l): Query<Lista>) -> Response {
    let kind = match tipo(l.kind.as_deref()) {
        Ok(k) => k,
        Err(msg) => return erro(StatusCode::BAD_REQUEST, msg),
    };
    let store = api.engine.store();
    let memorias = match (l.scope.as_deref(), projeto(l.project.as_deref())) {
        (Some("global"), _) => store.list(GLOBAL_PROJECT, kind),
        (_, Some(p)) => store.list_visible(p, kind),
        (_, None) => store.list_all(kind),
    };
    match memorias {
        Ok(ms) => Json(json!({ "ok": true, "memories": ms.iter().map(memoria_json).collect::<Vec<_>>() }))
            .into_response(),
        Err(e) => erro(StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")),
    }
}

async fn one(State(api): State<Api>, Path(id): Path<String>) -> Response {
    match api.engine.store().get(&id) {
        Ok(Some(m)) => Json(json!({ "ok": true, "memory": memoria_json(&m) })).into_response(),
        Ok(None) => erro(StatusCode::NOT_FOUND, "memória não encontrada"),
        Err(e) => erro(StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")),
    }
}

#[derive(Deserialize)]
struct Nova {
    project: Option<String>,
    scope: Option<String>,
    kind: String,
    title: String,
    #[serde(default)]
    body: String,
    #[serde(default)]
    priority: i64,
}

/// Criar pela API é gravar como DONO (quem tem o token é o dono).
async fn create(State(api): State<Api>, Json(n): Json<Nova>) -> Response {
    let kind = match MemoryKind::from_str(&n.kind) {
        Ok(k) => k,
        Err(e) => return erro(StatusCode::BAD_REQUEST, e.to_string()),
    };
    if let Err(msg) = validar(&n.title, &n.body, n.priority) {
        return erro(StatusCode::BAD_REQUEST, msg);
    }
    let nova = if n.scope.as_deref() == Some("global") {
        NewMemory::global(kind, &n.title, &n.body, n.priority)
    } else {
        match projeto(n.project.as_deref()) {
            Some(p) => NewMemory::user(p, kind, &n.title, &n.body, n.priority),
            None => return erro(StatusCode::BAD_REQUEST, "informe project, ou scope \"global\""),
        }
    };
    match api.engine.store().add(nova) {
        Ok(m) => {
            agendar_indice(&api);
            (StatusCode::CREATED, Json(json!({ "ok": true, "memory": memoria_json(&m) })))
                .into_response()
        }
        Err(e) => erro(StatusCode::BAD_REQUEST, format!("{e:#}")),
    }
}

#[derive(Deserialize)]
struct Edicao {
    kind: Option<String>,
    title: Option<String>,
    body: Option<String>,
    priority: Option<i64>,
}

async fn update(State(api): State<Api>, Path(id): Path<String>, Json(e): Json<Edicao>) -> Response {
    let atual = match api.engine.store().get(&id) {
        Ok(Some(m)) => m,
        Ok(None) => return erro(StatusCode::NOT_FOUND, "memória não encontrada"),
        Err(e) => return erro(StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")),
    };
    if atual.origin == Origin::Agent {
        return erro(
            StatusCode::FORBIDDEN,
            "memória de IA não se edita (a autora é a IA) — o dono pode apagá-la",
        );
    }
    let kind = match e.kind.as_deref() {
        Some(k) => match MemoryKind::from_str(k) {
            Ok(k) => k,
            Err(err) => return erro(StatusCode::BAD_REQUEST, err.to_string()),
        },
        None => atual.kind,
    };
    let title = e.title.unwrap_or(atual.title);
    let body = e.body.unwrap_or(atual.body);
    let priority = e.priority.unwrap_or(atual.priority);
    if let Err(msg) = validar(&title, &body, priority) {
        return erro(StatusCode::BAD_REQUEST, msg);
    }
    match api.engine.store().update(&id, kind, &title, &body, priority) {
        Ok(m) => {
            agendar_indice(&api);
            Json(json!({ "ok": true, "memory": memoria_json(&m) })).into_response()
        }
        Err(err) => erro(StatusCode::BAD_REQUEST, format!("{err:#}")),
    }
}

async fn remove(State(api): State<Api>, Path(id): Path<String>) -> Response {
    match api.engine.store().get(&id) {
        Ok(Some(_)) => {}
        Ok(None) => return erro(StatusCode::NOT_FOUND, "memória não encontrada"),
        Err(e) => return erro(StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")),
    }
    if let Err(e) = api.engine.store().delete(&id) {
        return erro(StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}"));
    }
    let engine = api.engine.clone();
    tokio::spawn(async move {
        engine
            .handle(daemon::Request::Remove { ids: vec![id] })
            .await;
    });
    Json(json!({ "ok": true })).into_response()
}

#[derive(Deserialize)]
struct Grafo {
    project: Option<String>,
}

async fn graph(State(api): State<Api>, Query(g): Query<Grafo>) -> Response {
    match api.engine.graph(projeto(g.project.as_deref())).await {
        Ok(v) => Json(v).into_response(),
        Err(e) => erro(StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")),
    }
}

#[derive(Deserialize)]
struct Contexto {
    project: String,
    prompt: String,
    author: Option<String>,
}

async fn context(State(api): State<Api>, Query(c): Query<Contexto>) -> Response {
    match api
        .engine
        .context(&c.project, &c.prompt, c.author.as_deref().unwrap_or("api"))
        .await
    {
        Ok((texto, semantic)) => {
            Json(json!({ "ok": true, "semantic": semantic, "text": texto })).into_response()
        }
        Err(e) => erro(StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use http_body_util::BodyExt;
    use orchestrator_memory::store::MemoryStore;
    use tower::ServiceExt;

    const PORTA: u16 = 10999;
    const TOKEN: &str = "0123456789abcdef0123456789abcdef";

    fn app() -> (Router, Arc<Engine>) {
        let store = MemoryStore::open_in_memory().unwrap();
        store
            .add(NewMemory::user("loja", MemoryKind::Architecture, "Banco é PostgreSQL", "migrações com sqlx", 3))
            .unwrap();
        let engine = Engine::new(store);
        (router(engine.clone(), PORTA, TOKEN.to_string()), engine)
    }

    fn pedido(method: Method, uri: &str) -> axum::http::request::Builder {
        axum::http::Request::builder()
            .method(method)
            .uri(uri)
            .header(header::HOST, format!("127.0.0.1:{PORTA}"))
    }

    async fn json_de(r: Response) -> Value {
        let bytes = r.into_body().collect().await.unwrap().to_bytes();
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    }

    fn com_json(b: axum::http::request::Builder, corpo: Value) -> axum::http::Request<Body> {
        b.header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(corpo.to_string()))
            .unwrap()
    }

    #[tokio::test]
    async fn health_answers_on_loopback() {
        let (app, _) = app();
        let r = app.oneshot(pedido(Method::GET, "/api/health").body(Body::empty()).unwrap()).await.unwrap();
        assert_eq!(r.status(), StatusCode::OK);
        let v = json_de(r).await;
        assert_eq!(v["ok"], json!(true));
        assert_eq!(v["memories"], json!(1));
        assert_eq!(v["semantic"], json!(false));
    }

    #[tokio::test]
    async fn a_foreign_host_is_refused_even_on_the_right_port() {
        for host in ["evil.example:10999", "127.0.0.1:80", "192.168.0.10:10999"] {
            let (app, _) = app();
            let r = app
                .oneshot(
                    axum::http::Request::builder()
                        .uri("/api/memories")
                        .header(header::HOST, host)
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(r.status(), StatusCode::MISDIRECTED_REQUEST, "{host}");
        }
    }

    #[tokio::test]
    async fn writing_without_the_right_token_is_refused() {
        let nova = json!({"project": "loja", "kind": "decision", "title": "invasão", "body": "x"});
        for auth in [None, Some("Bearer errado"), Some(TOKEN)] {
            let (app, engine) = app();
            let mut b = pedido(Method::POST, "/api/memories");
            if let Some(a) = auth {
                b = b.header(header::AUTHORIZATION, a);
            }
            let r = app.oneshot(com_json(b, nova.clone())).await.unwrap();
            assert_eq!(r.status(), StatusCode::UNAUTHORIZED, "{auth:?}");
            assert_eq!(engine.store().all_ids().unwrap().len(), 1);
        }
    }

    #[tokio::test]
    async fn the_owner_creates_edits_and_deletes_with_the_token() {
        let (app, engine) = app();
        let bearer = format!("Bearer {TOKEN}");
        let criar = pedido(Method::POST, "/api/memories").header(header::AUTHORIZATION, &bearer);
        let r = app
            .clone()
            .oneshot(com_json(criar, json!({"scope": "global", "kind": "practice", "title": "Commits em português", "priority": 9})))
            .await
            .unwrap();
        assert_eq!(r.status(), StatusCode::CREATED);
        let id = json_de(r).await["memory"]["id"].as_str().unwrap().to_string();
        assert_eq!(engine.store().get(&id).unwrap().unwrap().origin, Origin::User);

        let editar = pedido(Method::PUT, &format!("/api/memories/{id}")).header(header::AUTHORIZATION, &bearer);
        let r = app.clone().oneshot(com_json(editar, json!({"title": "Commits curtos em português"}))).await.unwrap();
        assert_eq!(r.status(), StatusCode::OK);
        assert_eq!(engine.store().get(&id).unwrap().unwrap().title, "Commits curtos em português");

        let apagar = pedido(Method::DELETE, &format!("/api/memories/{id}"))
            .header(header::AUTHORIZATION, &bearer)
            .body(Body::empty())
            .unwrap();
        assert_eq!(app.clone().oneshot(apagar).await.unwrap().status(), StatusCode::OK);
        let ler = pedido(Method::GET, &format!("/api/memories/{id}")).body(Body::empty()).unwrap();
        assert_eq!(app.oneshot(ler).await.unwrap().status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn ai_notes_cannot_be_edited_through_the_api() {
        let (app, engine) = app();
        let nota = engine
            .store()
            .add(NewMemory::agent("loja", "frontend", MemoryKind::Decision, "Vite", "x", 2))
            .unwrap();
        let b = pedido(Method::PUT, &format!("/api/memories/{}", nota.id))
            .header(header::AUTHORIZATION, format!("Bearer {TOKEN}"));
        let r = app.oneshot(com_json(b, json!({"title": "reescrita pelo dono"}))).await.unwrap();
        assert_eq!(r.status(), StatusCode::FORBIDDEN);
        assert_eq!(engine.store().get(&nota.id).unwrap().unwrap().title, "Vite");
    }

    #[tokio::test]
    async fn cors_only_allows_our_page_and_the_desktop_app() {
        for (origem, permitido) in [
            (format!("http://127.0.0.1:{PORTA}"), true),
            ("tauri://localhost".to_string(), true),
            ("http://evil.example".to_string(), false),
        ] {
            let (app, _) = app();
            let r = app
                .oneshot(
                    pedido(Method::OPTIONS, "/api/memories")
                        .header(header::ORIGIN, &origem)
                        .header(header::ACCESS_CONTROL_REQUEST_METHOD, "POST")
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            let liberou = r.headers().get(header::ACCESS_CONTROL_ALLOW_ORIGIN).is_some();
            assert_eq!(liberou, permitido, "{origem}");
        }
    }

    #[tokio::test]
    async fn search_without_models_falls_back_to_words_and_says_so() {
        let (app, _) = app();
        let r = app
            .oneshot(pedido(Method::GET, "/api/search?q=postgresql&project=loja").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(r.status(), StatusCode::OK);
        let v = json_de(r).await;
        assert_eq!(v["semantic"], json!(false));
        assert_eq!(v["hits"][0]["title"], json!("Banco é PostgreSQL"));
    }

    #[tokio::test]
    async fn the_graph_links_each_memory_to_its_hub() {
        let (app, _) = app();
        let r = app.oneshot(pedido(Method::GET, "/api/graph").body(Body::empty()).unwrap()).await.unwrap();
        let v = json_de(r).await;
        let ids: Vec<&str> = v["nodes"].as_array().unwrap().iter().filter_map(|n| n["id"].as_str()).collect();
        assert!(ids.contains(&"polo:loja"), "{ids:?}");
        assert!(v["links"].as_array().unwrap().iter().any(|l| l["target"] == json!("polo:loja")));
    }

    #[tokio::test]
    async fn the_page_is_served_with_a_strict_content_policy() {
        let (app, _) = app();
        let r = app.clone().oneshot(pedido(Method::GET, "/").body(Body::empty()).unwrap()).await.unwrap();
        assert_eq!(r.status(), StatusCode::OK);
        let tipo = r.headers().get(header::CONTENT_TYPE).unwrap().to_str().unwrap().to_string();
        assert!(tipo.starts_with("text/html"), "{tipo}");
        if Pagina::get("index.html").is_some() {
            let csp = r.headers().get(header::CONTENT_SECURITY_POLICY).unwrap().to_str().unwrap();
            assert!(csp.contains("script-src 'self'") && !csp.contains("unsafe-eval"), "{csp}");
        }
        let r = app
            .oneshot(pedido(Method::GET, "/assets/../../Cargo.toml").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(r.status(), StatusCode::NOT_FOUND, "fora da pasta embutida não sai nada");
    }

    #[test]
    fn the_token_file_is_created_private_and_reused() {
        let dir = tempfile::tempdir().unwrap();
        let caminho = dir.path().join("sub/memory-api.token");
        let a = load_or_create_token(&caminho).unwrap();
        assert_eq!(a.len(), 64);
        assert_eq!(load_or_create_token(&caminho).unwrap(), a);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let modo = std::fs::metadata(&caminho).unwrap().permissions().mode() & 0o777;
            assert_eq!(modo, 0o600);
        }
    }
}
