//! O ChromaDB do Orchestrator: um container podman, publicado só no
//! localhost, com os dados num volume do usuário.
//!
//! Falamos com ele pela API HTTP v2 (JSON), não pelo crate oficial `chroma`:
//! aquele exige o `protoc` instalado no sistema para compilar, o que pesaria
//! em todo build — inclusive nos pacotes AppImage/MinGW. As rotas e os
//! campos daqui foram conferidos na OpenAPI do próprio servidor (1.0.0) e
//! numa ida e volta real.
//!
//! O SQLite é a fonte da verdade; isto é um ÍNDICE reconstruível. Se o
//! container não sobe, o memoryd segue com candidatos léxicos + reranker.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context, Result};
use orchestrator_memory::Memory;
use serde_json::{json, Value};

use crate::models::{EMBED_DIM, EMBED_MODEL_NAME};

/// Imagem oficial fixada por DIGEST (servidor 1.0.0, verificado pela API).
/// A tag `latest` mudaria por baixo sem aviso — e o Docker Hub não publica
/// tags semver estáveis para esta imagem.
pub const IMAGE: &str =
    "docker.io/chromadb/chroma@sha256:abcce7c335e2dab9f11ef629296f7309b09cb19ae4b34da32ac7e34ff5773140";

/// Nome do container (encerrar é `podman kill orchestrator-chroma`).
pub const CONTAINER: &str = "orchestrator-chroma";

/// Porta no loopback do host.
pub const PORT: u16 = 8765;

/// Troca o servidor (testes, ou um Chroma que o usuário já tenha).
pub const URL_ENV: &str = "ORCHESTRATOR_CHROMA_URL";

/// Tenant e database padrão de um servidor local sem autenticação.
const TENANT_DB: &str = "/api/v2/tenants/default_tenant/databases/default_database";

/// Quanto esperar o servidor responder depois de subir o container.
const START_TIMEOUT: Duration = Duration::from_secs(90);

/// Prazo de cada chamada HTTP.
const HTTP_TIMEOUT: Duration = Duration::from_secs(15);

/// Uma coleção aberta.
#[derive(Clone)]
pub struct Collection {
    http: reqwest::Client,
    /// `.../collections/{id}`
    base: String,
}

/// Uma coleção por modelo E dimensão. O Chroma fixa a dimensão de uma
/// coleção no primeiro registro e a mantém mesmo depois de apagar tudo — uma
/// coleção homônima nascida com outra dimensão travava a indexação para
/// sempre ("expecting embedding with dimension of 4, got 384", visto ao vivo).
pub fn collection_name() -> String {
    format!("orch_memories_{}_{EMBED_DIM}", EMBED_MODEL_NAME.replace('-', "_"))
}

/// Endereço padrão do nosso container.
pub fn default_endpoint() -> String {
    format!("http://127.0.0.1:{PORT}")
}

/// Endereço em uso (`ORCHESTRATOR_CHROMA_URL` ou o padrão).
pub fn endpoint() -> String {
    std::env::var(URL_ENV)
        .ok()
        .map(|u| u.trim_end_matches('/').to_string())
        .filter(|u| !u.is_empty())
        .unwrap_or_else(default_endpoint)
}

/// Onde os dados do Chroma ficam no host.
pub fn data_dir() -> PathBuf {
    orchestrator_core::config::default_memory_db_path()
        .parent()
        .map(|d| d.join("chroma"))
        .unwrap_or_else(|| std::env::temp_dir().join("orchestrator-chroma"))
}

/// Argumentos do `podman run` — separados para o teste conferir que a porta
/// só abre no loopback e que a imagem está fixada.
pub fn run_args(dir: &Path) -> Vec<String> {
    vec![
        "run".into(),
        "-d".into(),
        "--rm".into(),
        "--replace".into(),
        "--name".into(),
        CONTAINER.into(),
        "-p".into(),
        format!("127.0.0.1:{PORT}:8000"),
        "-v".into(),
        format!("{}:/data:Z", dir.display()),
        IMAGE.into(),
    ]
}

/// Sobe o container (bloqueante).
fn start_container() -> Result<()> {
    let dir = data_dir();
    std::fs::create_dir_all(&dir).with_context(|| format!("criando {}", dir.display()))?;
    let out = Command::new("podman")
        .args(run_args(&dir))
        .output()
        .context("não consegui executar `podman` para subir o ChromaDB")?;
    if !out.status.success() {
        bail!(
            "podman run do ChromaDB falhou: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(())
}

fn http_client() -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .timeout(HTTP_TIMEOUT)
        // Proxy do ambiente não pode interceptar o localhost.
        .no_proxy()
        .build()
        .context("montando o cliente HTTP do ChromaDB")
}

async fn heartbeat(http: &reqwest::Client, url: &str) -> bool {
    http.get(format!("{url}/api/v2/heartbeat"))
        .timeout(Duration::from_millis(800))
        .send()
        .await
        .map(|r| r.status().is_success())
        .unwrap_or(false)
}

/// Texto legível de um erro do Chroma (`{"error", "message"}`).
pub fn describe_error(body: &Value) -> String {
    match (body["error"].as_str(), body["message"].as_str()) {
        (Some(e), Some(m)) => format!("{e}: {m}"),
        (None, Some(m)) => m.to_string(),
        _ => body.to_string(),
    }
}

async fn post_json(http: &reqwest::Client, url: &str, body: &Value) -> Result<Value> {
    let resposta = http
        .post(url)
        .json(body)
        .send()
        .await
        .with_context(|| format!("falando com o ChromaDB em {url}"))?;
    let status = resposta.status();
    let valor: Value = resposta.json().await.unwrap_or(Value::Null);
    if !status.is_success() {
        bail!("ChromaDB respondeu {status}: {}", describe_error(&valor));
    }
    Ok(valor)
}

/// Garante o servidor de pé e devolve a coleção das memórias.
///
/// Só sobe container quando o endereço é o nosso padrão — um servidor
/// apontado por `ORCHESTRATOR_CHROMA_URL` é de quem o configurou.
pub async fn connect() -> Result<Collection> {
    connect_collection(&collection_name()).await
}

/// Como [`connect`], para uma coleção com outro nome (testes).
pub async fn connect_collection(name: &str) -> Result<Collection> {
    let url = endpoint();
    let http = http_client()?;
    if !heartbeat(&http, &url).await {
        if url != default_endpoint() {
            bail!("o ChromaDB em {url} não responde");
        }
        tokio::task::spawn_blocking(start_container).await??;
        let inicio = Instant::now();
        while !heartbeat(&http, &url).await {
            if inicio.elapsed() > START_TIMEOUT {
                bail!("o ChromaDB não respondeu em {}s", START_TIMEOUT.as_secs());
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    }
    // Espaço padrão da coleção é l2; com vetores normalizados (o E5 devolve
    // assim) a ordem de vizinhos é a mesma do cosseno — e o reranker decide a
    // ordem final de qualquer forma.
    let criada = post_json(
        &http,
        &format!("{url}{TENANT_DB}/collections"),
        &json!({ "name": name, "get_or_create": true }),
    )
    .await
    .context("abrindo a coleção de memórias no ChromaDB")?;
    let id = criada["id"]
        .as_str()
        .ok_or_else(|| anyhow!("resposta sem id de coleção: {criada}"))?;
    Ok(Collection {
        http,
        base: format!("{url}{TENANT_DB}/collections/{id}"),
    })
}

/// Filtro "deste projeto OU global".
pub fn where_visible(project: &str) -> Value {
    json!({ "$or": [
        { "project": { "$eq": project } },
        { "scope": { "$eq": "global" } }
    ]})
}

/// Metadata gravada com o vetor.
pub fn metadata(m: &Memory) -> Value {
    json!({
        "project": m.project,
        "scope": m.scope.as_str(),
        "origin": m.origin.as_str(),
        "author": m.author,
        "kind": m.kind.as_str(),
        "priority": m.priority,
    })
}

/// O texto que representa a memória (título primeiro).
pub fn document(m: &Memory) -> String {
    format!("{}\n{}", m.title, m.body)
}

/// Corpo do upsert, com as listas alinhadas.
pub fn upsert_body(memories: &[Memory], vectors: &[Vec<f32>]) -> Value {
    json!({
        "ids": memories.iter().map(|m| m.id.clone()).collect::<Vec<_>>(),
        "embeddings": vectors,
        "documents": memories.iter().map(document).collect::<Vec<_>>(),
        "metadatas": memories.iter().map(metadata).collect::<Vec<_>>(),
    })
}

/// Corpo da consulta: só ids, filtrados pelo que o projeto enxerga.
pub fn query_body(vector: &[f32], n: u32, project: &str) -> Value {
    json!({
        "query_embeddings": [vector],
        "n_results": n,
        "where": where_visible(project),
        "include": [],
    })
}

/// Ids da primeira (e única) consulta de uma resposta de `query`.
pub fn parse_query_ids(body: &Value) -> Vec<String> {
    body["ids"][0]
        .as_array()
        .map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect())
        .unwrap_or_default()
}

/// Grava (ou regrava) memórias com os vetores dados.
pub async fn upsert(col: &Collection, memories: &[Memory], vectors: Vec<Vec<f32>>) -> Result<()> {
    if memories.is_empty() {
        return Ok(());
    }
    if memories.len() != vectors.len() {
        bail!("{} memórias para {} vetores", memories.len(), vectors.len());
    }
    post_json(
        &col.http,
        &format!("{}/upsert", col.base),
        &upsert_body(memories, &vectors),
    )
    .await
    .context("gravando no ChromaDB")?;
    Ok(())
}

/// Ids mais próximos do vetor, só entre o que o projeto enxerga.
pub async fn query(col: &Collection, vector: Vec<f32>, n: u32, project: &str) -> Result<Vec<String>> {
    let resposta = post_json(
        &col.http,
        &format!("{}/query", col.base),
        &query_body(&vector, n, project),
    )
    .await
    .context("consultando o ChromaDB")?;
    Ok(parse_query_ids(&resposta))
}

/// Corpo da consulta com distâncias; `project: None` procura em todos.
pub fn query_scored_body(vector: &[f32], n: u32, project: Option<&str>) -> Value {
    let mut corpo = json!({
        "query_embeddings": [vector],
        "n_results": n,
        "include": ["distances"],
    });
    if let Some(p) = project {
        corpo["where"] = where_visible(p);
    }
    corpo
}

/// Ids e similaridade de cosseno da primeira consulta.
///
/// A coleção usa l2 e o E5 devolve vetores normalizados, então a distância
/// que o Chroma entrega (l2 ao quadrado) vale `2 − 2·cos`.
pub fn parse_query_scored(body: &Value) -> Vec<(String, f32)> {
    let distancias = body["distances"][0].as_array().cloned().unwrap_or_default();
    parse_query_ids(body)
        .into_iter()
        .zip(distancias)
        .map(|(id, d)| (id, 1.0 - d.as_f64().unwrap_or(2.0) as f32 / 2.0))
        .collect()
}

/// Vizinhos mais próximos com similaridade; `project: None` = todos.
pub async fn query_scored(
    col: &Collection,
    vector: Vec<f32>,
    n: u32,
    project: Option<&str>,
) -> Result<Vec<(String, f32)>> {
    let resposta = post_json(
        &col.http,
        &format!("{}/query", col.base),
        &query_scored_body(&vector, n, project),
    )
    .await
    .context("consultando o ChromaDB")?;
    Ok(parse_query_scored(&resposta))
}

/// Os vetores gravados das memórias pedidas (o grafo liga vizinhos).
pub async fn embeddings(col: &Collection, ids: &[String]) -> Result<Vec<(String, Vec<f32>)>> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let resposta = post_json(
        &col.http,
        &format!("{}/get", col.base),
        &json!({ "ids": ids, "include": ["embeddings"] }),
    )
    .await
    .context("lendo vetores do ChromaDB")?;
    Ok(parse_embeddings(&resposta))
}

/// Ids e vetores de uma resposta de `get`.
pub fn parse_embeddings(body: &Value) -> Vec<(String, Vec<f32>)> {
    let ids = body["ids"].as_array().cloned().unwrap_or_default();
    let vetores = body["embeddings"].as_array().cloned().unwrap_or_default();
    ids.iter()
        .zip(vetores)
        .filter_map(|(id, v)| {
            let id = id.as_str()?.to_string();
            let v: Vec<f32> = v.as_array()?.iter().filter_map(|x| x.as_f64().map(|x| x as f32)).collect();
            (!v.is_empty()).then_some((id, v))
        })
        .collect()
}

/// Tira memórias do índice.
pub async fn remove(col: &Collection, ids: Vec<String>) -> Result<()> {
    if ids.is_empty() {
        return Ok(());
    }
    post_json(&col.http, &format!("{}/delete", col.base), &json!({ "ids": ids }))
        .await
        .context("apagando do ChromaDB")?;
    Ok(())
}

/// Todos os ids que o índice tem.
pub async fn all_ids(col: &Collection) -> Result<Vec<String>> {
    let resposta = post_json(&col.http, &format!("{}/get", col.base), &json!({ "include": [] }))
        .await
        .context("listando o ChromaDB")?;
    Ok(resposta["ids"]
        .as_array()
        .map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect())
        .unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use super::*;
    use orchestrator_memory::{MemoryKind, Origin, Scope};

    fn memoria(id: &str, project: &str, scope: Scope) -> Memory {
        Memory {
            id: id.into(),
            project: project.into(),
            kind: MemoryKind::Practice,
            scope,
            origin: Origin::Agent,
            author: "frontend".into(),
            title: format!("título {id}"),
            body: "corpo".into(),
            priority: 4,
            created_at: String::new(),
            updated_at: String::new(),
        }
    }

    #[test]
    fn the_container_only_listens_on_loopback_and_the_image_is_pinned() {
        let args = run_args(Path::new("/dados")).join(" ");
        assert!(args.contains("-p 127.0.0.1:8765:8000"), "{args}");
        assert!(!args.contains("0.0.0.0"), "{args}");
        assert!(args.contains("@sha256:"), "imagem sem digest: {args}");
        assert!(args.contains("/dados:/data:Z"), "{args}");
        assert!(args.contains("--name orchestrator-chroma"), "{args}");
    }

    #[test]
    fn the_filter_is_exactly_this_project_or_global() {
        assert_eq!(
            where_visible("loja"),
            json!({"$or": [{"project": {"$eq": "loja"}}, {"scope": {"$eq": "global"}}]})
        );
    }

    #[test]
    fn bodies_keep_ids_vectors_documents_and_metadata_aligned() {
        let ms = vec![memoria("a", "loja", Scope::Project), memoria("b", "*", Scope::Global)];
        let corpo = upsert_body(&ms, &[vec![1.0, 0.0], vec![0.0, 1.0]]);
        assert_eq!(corpo["ids"], json!(["a", "b"]));
        assert_eq!(corpo["documents"][0], json!("título a\ncorpo"));
        assert_eq!(corpo["metadatas"][1]["scope"], json!("global"));
        assert_eq!(corpo["metadatas"][0]["author"], json!("frontend"));
        assert_eq!(corpo["metadatas"][0]["priority"], json!(4));
        let q = query_body(&[1.0, 0.0], 24, "loja");
        assert_eq!(q["n_results"], json!(24));
        assert_eq!(q["include"], json!([]));
        // Modelo novo, coleção nova: vetores do fp32 e do int8 não se misturam.
        assert_eq!(collection_name(), "orch_memories_multilingual_e5_small_int8_384");
    }

    #[test]
    fn scored_queries_turn_distances_into_cosine_and_can_search_everything() {
        let um = query_scored_body(&[1.0, 0.0], 5, Some("loja"));
        assert_eq!(um["include"], json!(["distances"]));
        assert_eq!(um["where"], where_visible("loja"));
        let todos = query_scored_body(&[1.0, 0.0], 5, None);
        assert!(todos.get("where").is_none(), "{todos}");
        let achados = parse_query_scored(&json!({"ids": [["a", "b"]], "distances": [[0.2, 1.0]]}));
        assert_eq!(achados.len(), 2);
        assert!((achados[0].1 - 0.9).abs() < 1e-6 && (achados[1].1 - 0.5).abs() < 1e-6, "{achados:?}");
        let vetores = parse_embeddings(&json!({"ids": ["a", "b"], "embeddings": [[0.5, 0.5], null]}));
        assert_eq!(vetores, [("a".to_string(), vec![0.5, 0.5])]);
    }

    #[test]
    fn responses_and_errors_are_read_defensively() {
        assert_eq!(parse_query_ids(&json!({"ids": [["a", "b"]]})), ["a", "b"]);
        assert!(parse_query_ids(&json!({"ids": []})).is_empty());
        assert!(parse_query_ids(&Value::Null).is_empty());
        assert_eq!(
            describe_error(&json!({"error": "InvalidArgumentError", "message": "dimension of 3, got 2"})),
            "InvalidArgumentError: dimension of 3, got 2"
        );
    }

    /// Ida e volta contra um servidor REAL. Liga com `ORCH_CHROMA_TEST=1` e
    /// `ORCHESTRATOR_CHROMA_URL` apontando para um Chroma descartável.
    #[tokio::test]
    async fn round_trip_against_a_real_server() {
        if std::env::var("ORCH_CHROMA_TEST").as_deref() != Ok("1") {
            eprintln!("pulado: defina ORCH_CHROMA_TEST=1 e ORCHESTRATOR_CHROMA_URL");
            return;
        }
        // NUNCA a coleção de produção: estes vetores de 4 dimensões fixariam
        // a dimensão dela e quebrariam a indexação real (aconteceu).
        let col = connect_collection("orch_teste_ida_e_volta")
            .await
            .expect("conectar no Chroma de teste");
        let ms = vec![
            memoria("rt-loja", "loja", Scope::Project),
            memoria("rt-global", "*", Scope::Global),
            memoria("rt-outro", "outro", Scope::Project),
        ];
        let vetores = vec![vec![1.0, 0.0, 0.0, 0.0], vec![0.9, 0.1, 0.0, 0.0], vec![1.0, 0.0, 0.0, 0.0]];
        upsert(&col, &ms, vetores).await.unwrap();
        let achados = query(&col, vec![1.0, 0.0, 0.0, 0.0], 10, "loja").await.unwrap();
        assert!(achados.contains(&"rt-loja".to_string()), "{achados:?}");
        assert!(achados.contains(&"rt-global".to_string()), "{achados:?}");
        assert!(!achados.contains(&"rt-outro".to_string()), "vazou outro projeto: {achados:?}");
        remove(&col, vec!["rt-global".into()]).await.unwrap();
        let ids = all_ids(&col).await.unwrap();
        assert!(!ids.contains(&"rt-global".to_string()) && ids.contains(&"rt-loja".to_string()));
        remove(&col, vec!["rt-loja".into(), "rt-outro".into()]).await.unwrap();
    }
}
