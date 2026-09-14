//! A busca híbrida e a indexação.
//!
//! Candidatos vêm de DOIS lados — o vizinho vetorial no ChromaDB (acha o que
//! fala do mesmo assunto com outras palavras) e o léxico do SQLite (acha o
//! termo exato que o vetor às vezes perde). A lista mesclada passa pelo
//! reranker cross-encoder, que lê prompt e memória juntos, e o score final
//! leva os pesos de tipo, prioridade e origem (dono acima de IA).
//!
//! Tudo que falta degrada em vez de falhar: sem Chroma, só candidatos
//! léxicos (ainda reranqueados); sem modelos, o caminho léxico inteiro.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use anyhow::{anyhow, Result};
use orchestrator_memory::contract::{self, ContractInput, Retrieval, Scored};
use orchestrator_memory::daemon::{Hit, Request, Response};
use orchestrator_memory::relevance;
use orchestrator_memory::rerank::{cross_score, weighted};
use orchestrator_memory::store::MemoryStore;
use orchestrator_memory::{Memory, MemoryKind};
use std::str::FromStr;
use tokio::sync::RwLock;

use crate::chroma::{self, Collection};
use crate::download::SharedProgress;
use crate::models::{Models, EMBED_MODEL_NAME};

/// Vizinhos vetoriais pedidos ao Chroma.
///
/// Com o gte cada candidato custa ~20 ms no reranker (medido): 10 vetoriais +
/// 4 léxicos mantêm a busca completa perto de 250-300 ms.
const VECTOR_CANDIDATES: u32 = 10;
/// Candidatos léxicos somados aos vetoriais.
const LEXICAL_CANDIDATES: usize = 4;
/// Memórias embedadas por lote na indexação.
const INDEX_BATCH: usize = 32;

/// Relevância CRUA mínima (0..1 do cross-encoder) para a MELHOR memória entrar.
///
/// Medido com o gte (`examples/bench.rs --diagnostico`): pergunta sem relação
/// nenhuma ("capital da França", "futebol") não passou de 0,17; pedido de
/// tarefa com memória certa ficou a partir de 0,22 ("vou criar a tabela de
/// pedidos no banco" → PostgreSQL 0,223).
pub const MIN_RELEVANCE: f32 = 0.20;

/// As DEMAIS memórias precisam de relevância absoluta maior...
///
/// Um corte único não serve: no mesmo benchmark, distrações chegaram a 0,35 em
/// alguns prompts enquanto a memória certa ficava em 0,22 em outros.
pub const SECONDARY_RELEVANCE: f32 = 0.30;

/// ...e de estar perto da melhor: fração mínima da relevância dela.
pub const RELATIVE_TO_BEST: f32 = 0.80;

/// O estado do serviço.
pub struct Engine {
    store: MemoryStore,
    models: OnceLock<Models>,
    collection: RwLock<Option<Collection>>,
    indexing: tokio::sync::Mutex<()>,
    last_activity: Mutex<Instant>,
    /// Andamento do download dos modelos (1ª execução).
    pub download: SharedProgress,
}

impl Engine {
    pub fn new(store: MemoryStore) -> Arc<Self> {
        Arc::new(Self {
            store,
            models: OnceLock::new(),
            collection: RwLock::new(None),
            indexing: tokio::sync::Mutex::new(()),
            last_activity: Mutex::new(Instant::now()),
            download: SharedProgress::default(),
        })
    }

    /// O banco (fonte da verdade das memórias).
    pub fn store(&self) -> &MemoryStore {
        &self.store
    }

    /// O que um projeto enxerga — ou tudo, com `None`.
    fn visible(&self, project: Option<&str>, kind: Option<MemoryKind>) -> Result<Vec<Memory>> {
        match project {
            Some(p) => self.store.list_visible(p, kind),
            None => self.store.list_all(kind),
        }
    }

    pub fn set_models(&self, models: Models) {
        let _ = self.models.set(models);
    }

    pub fn models_ready(&self) -> bool {
        self.models.get().is_some()
    }

    pub async fn set_collection(&self, col: Collection) {
        *self.collection.write().await = Some(col);
    }

    /// Esquece a conexão atual; a próxima volta do laço reconecta.
    pub async fn clear_collection(&self) {
        *self.collection.write().await = None;
    }

    pub async fn has_collection(&self) -> bool {
        self.collection.read().await.is_some()
    }

    async fn collection(&self) -> Option<Collection> {
        self.collection.read().await.clone()
    }

    /// Marca atividade (o memoryd sai sozinho depois de muito tempo ocioso).
    pub fn touch(&self) {
        if let Ok(mut t) = self.last_activity.lock() {
            *t = Instant::now();
        }
    }

    /// Há quanto tempo ninguém pede nada.
    pub fn idle_for(&self) -> Duration {
        self.last_activity
            .lock()
            .map(|t| t.elapsed())
            .unwrap_or_default()
    }

    /// Atende uma requisição.
    pub async fn handle(self: &Arc<Self>, request: Request) -> Response {
        self.touch();
        let resultado = match request {
            Request::Health => Ok(Response::ok_text(
                self.status().await,
                self.models_ready() && self.has_collection().await,
            )),
            Request::Context {
                project,
                prompt,
                author,
            } => self
                .context(&project, &prompt, &author)
                .await
                .map(|(texto, semantic)| Response::ok_text(texto, semantic)),
            Request::Search {
                project,
                query,
                top_k,
                kind,
            } => {
                let kind = match kind.as_deref().map(MemoryKind::from_str) {
                    Some(Ok(k)) => Some(k),
                    Some(Err(e)) => return Response::failed(e.to_string()),
                    None => None,
                };
                self.ranked(Some(&project), &query, top_k, kind)
                    .await
                    .map(|(ranked, semantic)| Response {
                        ok: true,
                        hits: ranked
                            .into_iter()
                            .map(|s| Hit {
                                id: s.memory.id,
                                score: s.score,
                            })
                            .collect(),
                        semantic,
                        ..Response::default()
                    })
            }
            Request::Index { .. } => {
                // Indexa TUDO que está pendente — cobre os ids pedidos e o que
                // outro processo gravou sem avisar.
                let me = self.clone();
                tokio::spawn(async move {
                    if let Err(e) = me.index_pending().await {
                        tracing::warn!("indexação falhou: {e:#}");
                    }
                });
                Ok(Response::ok_text("indexação agendada", false))
            }
            Request::Remove { ids } => match self.collection().await {
                Some(col) => chroma::remove(&col, ids)
                    .await
                    .map(|_| Response::ok_text("removidas do índice", true)),
                // Sem índice agora: a sobra é descartada na hora da busca
                // (id que não existe mais no SQLite não vira candidato).
                None => Ok(Response::ok_text("índice indisponível; nada a remover", false)),
            },
            Request::Reindex => self
                .reindex()
                .await
                .map(|texto| Response::ok_text(texto, true)),
        };
        resultado.unwrap_or_else(|e| Response::failed(format!("{e:#}")))
    }

    /// Estado dos modelos em palavras: prontos, baixando (com %) ou carregando.
    pub fn models_state(&self) -> String {
        if self.models_ready() {
            return "prontos".into();
        }
        match self.download.lock() {
            Ok(p) if !p.concluido && p.total > 0 => format!("baixando {}%", p.percent()),
            _ => "carregando".into(),
        }
    }

    /// Uma linha de estado.
    pub async fn status(&self) -> String {
        format!(
            "memoryd pid {} · modelos: {} · chroma: {}",
            std::process::id(),
            self.models_state(),
            if self.has_collection().await {
                "conectado"
            } else {
                "indisponível"
            }
        )
    }

    /// As memórias mais relevantes para `query` e se a busca foi semântica.
    pub async fn ranked(
        self: &Arc<Self>,
        project: Option<&str>,
        query: &str,
        top_k: usize,
        kind: Option<MemoryKind>,
    ) -> Result<(Vec<Scored>, bool)> {
        let visible = self.visible(project, kind)?;
        if visible.is_empty() || top_k == 0 {
            return Ok((Vec::new(), false));
        }
        let Some(_) = self.models.get() else {
            let scored = relevance::rank_relevant(query, visible, top_k)
                .into_iter()
                .map(|r| Scored {
                    memory: r.memory,
                    score: r.score,
                })
                .collect();
            return Ok((scored, false));
        };

        let lexical: Vec<String> = relevance::rank_relevant(query, visible.clone(), LEXICAL_CANDIDATES)
            .into_iter()
            .map(|r| r.memory.id)
            .collect();
        let mut vector_ids = Vec::new();
        let mut chroma_used = false;
        if let Some(col) = self.collection().await {
            let me = self.clone();
            let q = query.to_string();
            let vetor = tokio::task::spawn_blocking(move || me.models().embed_query(&q)).await??;
            match chroma::query_scored(&col, vetor, VECTOR_CANDIDATES, project).await {
                Ok(achados) => {
                    vector_ids = achados.into_iter().map(|(id, _)| id).collect();
                    chroma_used = true;
                }
                Err(e) => tracing::warn!("consulta ao Chroma falhou, sigo com léxico: {e:#}"),
            }
        }

        let candidates = merge_candidates(&vector_ids, &lexical, &visible);
        let docs: Vec<String> = candidates.iter().map(chroma::document).collect();
        let me = self.clone();
        let q = query.to_string();
        let scores = tokio::task::spawn_blocking(move || me.models().rerank(&q, &docs)).await??;
        Ok((score_candidates(&candidates, scores, top_k), chroma_used))
    }

    fn models(&self) -> &Models {
        self.models.get().expect("chamado só com modelos carregados")
    }

    /// Busca da API: um projeto (com as globais) ou todos.
    ///
    /// `rerank = false` é a busca enquanto a pessoa digita: só o vetor, em
    /// milissegundos, com a similaridade de cosseno como score. `true` passa
    /// pelo reranker e pelo corte de relevância (o mesmo caminho do prompt).
    pub async fn search(
        self: &Arc<Self>,
        project: Option<&str>,
        query: &str,
        top_k: usize,
        kind: Option<MemoryKind>,
        rerank: bool,
    ) -> Result<SearchOutcome> {
        self.touch();
        let top_k = top_k.clamp(1, 50);
        if rerank {
            let (hits, semantic) = self.ranked(project, query, top_k, kind).await?;
            return Ok(SearchOutcome {
                hits,
                semantic,
                reranked: self.models_ready(),
            });
        }
        let visible = self.visible(project, kind)?;
        if let (true, Some(col)) = (self.models_ready(), self.collection().await) {
            let me = self.clone();
            let q = query.to_string();
            let vetor = tokio::task::spawn_blocking(move || me.models().embed_query(&q)).await??;
            let achados =
                chroma::query_scored(&col, vetor, (top_k * 3).min(150) as u32, project).await?;
            let por_id: HashMap<&str, &Memory> = visible.iter().map(|m| (m.id.as_str(), m)).collect();
            let hits = achados
                .into_iter()
                .filter_map(|(id, score)| {
                    por_id.get(id.as_str()).map(|m| Scored {
                        memory: (*m).clone(),
                        score,
                    })
                })
                .take(top_k)
                .collect();
            return Ok(SearchOutcome {
                hits,
                semantic: true,
                reranked: false,
            });
        }
        let hits = relevance::rank_relevant(query, visible, top_k)
            .into_iter()
            .map(|r| Scored {
                memory: r.memory,
                score: r.score,
            })
            .collect();
        Ok(SearchOutcome {
            hits,
            semantic: false,
            reranked: false,
        })
    }

    /// Grafo para o globo: polos (global e cada projeto), memórias ligadas ao
    /// seu polo e, com o índice de pé, aos vizinhos de sentido mais próximos.
    pub async fn graph(self: &Arc<Self>, project: Option<&str>) -> Result<serde_json::Value> {
        use serde_json::json;
        self.touch();
        let memorias = self.visible(project, None)?;
        let polo = |m: &Memory| {
            if m.scope == orchestrator_memory::Scope::Global {
                "polo:global".to_string()
            } else {
                format!("polo:{}", m.project)
            }
        };
        let mut polos: Vec<String> = memorias.iter().map(polo).collect();
        polos.sort();
        polos.dedup();
        let mut nodes: Vec<serde_json::Value> = polos
            .iter()
            .map(|p| {
                let nome = p.trim_start_matches("polo:");
                let contagem = memorias.iter().filter(|m| polo(m) == *p).count();
                json!({"id": p, "type": "hub", "label": if nome == "global" { "Global" } else { nome }, "count": contagem})
            })
            .collect();
        let mut links: Vec<serde_json::Value> = Vec::new();
        for m in &memorias {
            nodes.push(json!({
                "id": m.id, "type": "memory", "label": m.title, "kind": m.kind.as_str(),
                "project": m.project, "scope": m.scope.as_str(), "origin": m.origin.as_str(),
                "author": m.author, "priority": m.priority,
            }));
            links.push(json!({"source": m.id, "target": polo(m), "type": "branch"}));
        }
        let mut semantic = false;
        if let Some(col) = self.collection().await {
            let ids: Vec<String> = memorias.iter().map(|m| m.id.clone()).collect();
            if let Ok(vetores) = chroma::embeddings(&col, &ids).await {
                semantic = !vetores.is_empty();
                for (a, b, sim) in semantic_neighbors(&vetores, GRAPH_NEIGHBORS, GRAPH_MIN_SIMILARITY) {
                    links.push(json!({"source": a, "target": b, "type": "semantic", "weight": sim}));
                }
            }
        }
        Ok(json!({"nodes": nodes, "links": links, "semantic": semantic}))
    }

    /// O texto invisível completo para um prompt.
    pub async fn context(
        self: &Arc<Self>,
        project: &str,
        prompt: &str,
        author: &str,
    ) -> Result<(String, bool)> {
        let visible = self.store.list_visible(project, None)?;
        let fixed = contract::pinned(&visible);
        let (ranked, semantic) = self
            .ranked(Some(project), prompt, contract::TOP_INDEX + fixed.len(), None)
            .await?;
        let index = contract::index_without_pinned(ranked, &fixed, contract::TOP_INDEX);
        let texto = contract::render(&ContractInput {
            project,
            author,
            pinned: &fixed,
            index: &index,
            security_rules: contract::security_count(&visible),
            retrieval: if semantic {
                Retrieval::Semantic
            } else {
                Retrieval::Lexical
            },
        });
        Ok((texto, semantic))
    }

    /// Indexa o que está pendente. Devolve quantas memórias entraram.
    pub async fn index_pending(self: &Arc<Self>) -> Result<usize> {
        let _vez = self.indexing.lock().await;
        let (true, Some(col)) = (self.models_ready(), self.collection().await) else {
            return Ok(0);
        };
        let pendentes = self.store.pending_index(EMBED_MODEL_NAME)?;
        let mut total = 0;
        for lote in pendentes.chunks(INDEX_BATCH) {
            let docs: Vec<String> = lote.iter().map(chroma::document).collect();
            let me = self.clone();
            let vetores =
                tokio::task::spawn_blocking(move || me.models().embed_passages(&docs)).await??;
            chroma::upsert(&col, lote, vetores).await?;
            let ids: Vec<String> = lote.iter().map(|m| m.id.clone()).collect();
            self.store.mark_indexed(&ids, EMBED_MODEL_NAME)?;
            total += lote.len();
        }
        if total > 0 {
            tracing::info!("{total} memória(s) indexada(s)");
        }
        Ok(total)
    }

    /// Reconstrói o índice a partir do SQLite, podando o que sobrou de
    /// memórias apagadas.
    pub async fn reindex(self: &Arc<Self>) -> Result<String> {
        if !self.models_ready() {
            return Err(anyhow!("os modelos ainda estão carregando — tente de novo em instantes"));
        }
        let col = self
            .collection()
            .await
            .ok_or_else(|| anyhow!("o ChromaDB não está disponível"))?;
        let no_indice = chroma::all_ids(&col).await?;
        let vivos = self.store.existing_ids(&no_indice)?;
        let sobras: Vec<String> = no_indice.into_iter().filter(|id| !vivos.contains(id)).collect();
        let podadas = sobras.len();
        chroma::remove(&col, sobras).await?;
        self.store.clear_indexed()?;
        let n = self.index_pending().await?;
        Ok(format!(
            "índice reconstruído: {n} memória(s) indexada(s), {podadas} sobra(s) removida(s)"
        ))
    }
}

/// Resultado de [`Engine::search`].
pub struct SearchOutcome {
    pub hits: Vec<Scored>,
    /// Veio do vetor/reranker (e não do caminho por palavras)?
    pub semantic: bool,
    /// Passou pelo reranker?
    pub reranked: bool,
}

/// Vizinhos de sentido por memória no grafo.
const GRAPH_NEIGHBORS: usize = 3;
/// Similaridade mínima (cosseno do E5) para ligar duas memórias no grafo.
const GRAPH_MIN_SIMILARITY: f32 = 0.86;

/// Pares `(a, b, similaridade)` dos `k` vizinhos mais próximos de cada vetor
/// acima do mínimo, sem repetir o par nas duas direções.
pub fn semantic_neighbors(vetores: &[(String, Vec<f32>)], k: usize, min: f32) -> Vec<(String, String, f32)> {
    let mut pares: Vec<(String, String, f32)> = Vec::new();
    let mut vistos = HashSet::new();
    for (i, (a, va)) in vetores.iter().enumerate() {
        let mut vizinhos: Vec<(usize, f32)> = vetores
            .iter()
            .enumerate()
            .filter(|(j, _)| *j != i)
            .map(|(j, (_, vb))| (j, orchestrator_memory::rerank::cosine_similarity(va, vb)))
            .filter(|(_, s)| *s >= min)
            .collect();
        vizinhos.sort_by(|x, y| y.1.partial_cmp(&x.1).unwrap_or(std::cmp::Ordering::Equal));
        for (j, s) in vizinhos.into_iter().take(k) {
            let b = &vetores[j].0;
            let chave = if a < b { (a.clone(), b.clone()) } else { (b.clone(), a.clone()) };
            if vistos.insert(chave) {
                pares.push((a.clone(), b.clone(), s));
            }
        }
    }
    pares
}

/// Aplica o corte de relevância e os pesos aos scores do reranker.
///
/// O corte olha a relevância CRUA, antes dos pesos: prioridade e tipo ORDENAM
/// o que tem a ver com o prompt, não decidem se tem a ver. A melhor entra a
/// partir de [`MIN_RELEVANCE`]; as outras precisam de [`SECONDARY_RELEVANCE`]
/// e de ficar a [`RELATIVE_TO_BEST`] da melhor.
pub fn score_candidates(candidates: &[Memory], scores: Vec<(usize, f32)>, top_k: usize) -> Vec<Scored> {
    let relevancias: Vec<(usize, f32)> = scores
        .into_iter()
        .map(|(i, logit)| (i, cross_score(logit)))
        .collect();
    let melhor = relevancias.iter().map(|(_, r)| *r).fold(f32::MIN, f32::max);
    let corte_demais = SECONDARY_RELEVANCE.max(melhor * RELATIVE_TO_BEST);
    let mut scored: Vec<Scored> = relevancias
        .into_iter()
        .filter_map(|(i, relevancia)| {
            let entra = if relevancia >= melhor {
                relevancia >= MIN_RELEVANCE
            } else {
                relevancia >= corte_demais
            };
            if !entra {
                return None;
            }
            candidates.get(i).map(|m| Scored {
                score: weighted(relevancia, m),
                memory: m.clone(),
            })
        })
        .collect();
    scored.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
    scored.truncate(top_k);
    scored
}

/// Mescla os candidatos vetoriais e léxicos, na ordem, sem repetir, e só com
/// memórias que ainda existem e que o projeto enxerga — o índice pode ter
/// sobra de algo apagado enquanto o memoryd estava fora.
pub fn merge_candidates(vector_ids: &[String], lexical_ids: &[String], visible: &[Memory]) -> Vec<Memory> {
    let por_id: HashMap<&str, &Memory> = visible.iter().map(|m| (m.id.as_str(), m)).collect();
    let mut vistos = HashSet::new();
    let mut saida = Vec::new();
    for id in vector_ids.iter().chain(lexical_ids) {
        if let Some(m) = por_id.get(id.as_str()) {
            if vistos.insert(id.as_str()) {
                saida.push((*m).clone());
            }
        }
    }
    saida
}

#[cfg(test)]
mod tests {
    use super::*;
    use orchestrator_memory::store::NewMemory;

    fn ids(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn merging_keeps_vector_order_dedups_and_drops_what_no_longer_exists() {
        let store = MemoryStore::open_in_memory().unwrap();
        let a = store.add_memory("p", MemoryKind::Syntax, "a", "x", 0).unwrap();
        let b = store.add_memory("p", MemoryKind::Syntax, "b", "x", 0).unwrap();
        let c = store.add_memory("p", MemoryKind::Syntax, "c", "x", 0).unwrap();
        let visible = store.list_visible("p", None).unwrap();
        let merged = merge_candidates(
            &ids(&[&b.id, "apagada-no-sqlite", &a.id]),
            &ids(&[&a.id, &c.id]),
            &visible,
        );
        let titulos: Vec<&str> = merged.iter().map(|m| m.title.as_str()).collect();
        assert_eq!(titulos, ["b", "a", "c"]);
    }

    #[test]
    fn irrelevant_memories_stay_out_even_with_high_priority() {
        let store = MemoryStore::open_in_memory().unwrap();
        let seg = store.add_memory("p", MemoryKind::Security, "nunca rm", "deny", 10).unwrap();
        let banco = store.add_memory("p", MemoryKind::Architecture, "postgres", "sqlx", 3).unwrap();
        let vite = store
            .add(NewMemory::agent("p", "front", MemoryKind::Decision, "vite", "x", 2))
            .unwrap();
        let logit = |p: f32| (p / (1.0 - p)).ln();
        let cands = vec![seg, banco, vite];
        // Relevâncias cruas do gte no benchmark para "qual SGBD a gente usa?":
        // a certa 0,494, a melhor distração 0,315, uma sem relação 0,12.
        let achados = score_candidates(
            &cands,
            vec![(0, logit(0.12)), (1, logit(0.494)), (2, logit(0.315))],
            4,
        );
        let titulos: Vec<&str> = achados.iter().map(|s| s.memory.title.as_str()).collect();
        assert_eq!(titulos, ["postgres"], "a regra p10 sem relação e a distração ficam de fora");
        // Pedido de tarefa: a certa vem baixa (0,223), mas é a melhor e entra.
        let tarefa = score_candidates(
            &cands,
            vec![(0, logit(0.139)), (1, logit(0.223)), (2, logit(0.148))],
            4,
        );
        assert_eq!(tarefa.len(), 1);
        assert_eq!(tarefa[0].memory.title, "postgres");
        // Duas quase empatadas e altas: as duas entram.
        let empate = score_candidates(&cands, vec![(1, logit(0.404)), (2, logit(0.393))], 4);
        assert_eq!(empate.len(), 2);
        // Pergunta sem relação nenhuma (as maiores do gte: 0,168 e 0,157):
        // índice vazio, e não completado à força.
        assert!(score_candidates(&cands, vec![(0, logit(0.168)), (1, logit(0.157))], 4).is_empty());
    }

    #[tokio::test]
    async fn without_models_the_context_is_lexical_and_still_complete() {
        let store = MemoryStore::open_in_memory().unwrap();
        store
            .add(NewMemory::global(MemoryKind::Practice, "commits em português", "sempre", 9))
            .unwrap();
        store
            .add(NewMemory::user("loja", MemoryKind::Architecture, "banco postgres", "sqlx", 2))
            .unwrap();
        let engine = Engine::new(store);
        let (texto, semantic) = engine
            .context("loja", "qual banco usamos?", "backend")
            .await
            .unwrap();
        assert!(!semantic);
        assert!(texto.contains("palavras-chave"), "{texto}");
        assert!(texto.contains("REGRAS FIXAS"), "{texto}");
        assert!(texto.contains("commits em português"), "{texto}");
        assert!(texto.contains("banco postgres"), "{texto}");

        let r = engine
            .handle(Request::Search {
                project: "loja".into(),
                query: "banco".into(),
                top_k: 3,
                kind: None,
            })
            .await;
        assert!(r.ok && !r.semantic);
        assert!(!r.hits.is_empty());

        let saude = engine.handle(Request::Health).await;
        assert!(saude.ok && saude.text.contains("carregando"), "{}", saude.text);
        // Reindex sem modelos recusa explicando, em vez de fingir.
        let re = engine.handle(Request::Reindex).await;
        assert!(!re.ok && re.error.unwrap().contains("carregando"));
        // Tipo inválido na busca é erro claro.
        let ruim = engine
            .handle(Request::Search {
                project: "loja".into(),
                query: "x".into(),
                top_k: 1,
                kind: Some("banana".into()),
            })
            .await;
        assert!(!ruim.ok);
    }
}
