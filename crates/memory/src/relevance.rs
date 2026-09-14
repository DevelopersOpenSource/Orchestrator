//! Seleção das memórias que valem o token: dado o que o usuário acabou de
//! escrever, ranqueia e devolve só as melhores.
//!
//! É o caminho de RESERVA: o principal é o semântico, no
//! `orchestrator-memoryd` (ChromaDB + reranker). Este roda em qualquer
//! processo, sem modelo — quando o memoryd não responde a tempo, o prompt
//! segue com este índice em vez de ficar sem nenhum. O sinal é sobreposição
//! de termos (com IDF, para palavra comum pesar pouco) combinada com os
//! mesmos pesos de tipo, prioridade e origem de [`crate::rerank`].
//!
//! O resultado é um ÍNDICE (título + resumo curto), não o corpo inteiro: o
//! orquestrador lê o que interessar com a tool `retrieve_memory`.

use std::collections::HashMap;

use crate::rerank::{priority_boost, weighted};
use crate::{Memory, MemoryKind};

/// Piso de score das memórias de segurança: as regras do dono aparecem no
/// índice mesmo quando a mensagem não menciona nada parecido.
const SECURITY_FLOOR: f32 = 0.35;

/// Palavras que não discriminam nada (PT + EN), ignoradas no casamento.
const STOPWORDS: &[&str] = &[
    "a", "as", "o", "os", "um", "uma", "de", "do", "da", "dos", "das", "em", "no", "na", "nos",
    "nas", "por", "para", "pra", "com", "sem", "que", "se", "e", "ou", "ao", "aos", "à", "às",
    "isso", "isto", "esse", "essa", "este", "esta", "eu", "voce", "você", "ele", "ela", "meu",
    "minha", "seu", "sua", "the", "of", "to", "in", "on", "for", "and", "or", "is", "are", "be",
    "it", "this", "that", "with", "as", "at", "an", "my", "your",
];

/// Uma memória escolhida para o índice, com o score que a colocou lá.
#[derive(Debug, Clone)]
pub struct Relevant {
    pub memory: Memory,
    pub score: f32,
}

/// Ranqueia `memories` contra `query` e devolve as `top_k` melhores.
///
/// Memórias sem nenhuma sobreposição com a consulta ainda podem entrar por
/// tipo/prioridade (uma regra de segurança prioritária vale mais que uma
/// nota de sintaxe que casou uma palavra à toa), mas o corte final é `top_k`.
pub fn rank(query: &str, memories: Vec<Memory>, top_k: usize) -> Vec<Relevant> {
    if memories.is_empty() || top_k == 0 {
        return Vec::new();
    }
    let terms = tokenize(query);
    let idf = idf_table(&memories);

    let mut scored: Vec<Relevant> = memories
        .into_iter()
        .map(|memory| {
            let overlap = overlap_score(&terms, &memory, &idf);
            let mut score = weighted(overlap, &memory);
            if memory.kind == MemoryKind::Security {
                score = score.max(SECURITY_FLOOR * priority_boost(memory.priority));
            }
            Relevant { memory, score }
        })
        .collect();

    scored.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            // Empate: prioridade maior, depois título (determinismo).
            .then(b.memory.priority.cmp(&a.memory.priority))
            .then(a.memory.title.cmp(&b.memory.title))
    });
    scored.truncate(top_k);
    scored
}

/// Como [`rank`], mas só com o que tem alguma relação com a pergunta.
///
/// É o que chega ao dono e ao índice invisível: completar a lista com
/// memória que não casa nada (score 0) faria o índice sugerir que ela
/// importa — o certo é dizer que não há nada relevante. Regra de segurança
/// continua entrando pelo piso de score.
pub fn rank_relevant(query: &str, memories: Vec<Memory>, top_k: usize) -> Vec<Relevant> {
    rank(query, memories, top_k)
        .into_iter()
        .filter(|r| r.score > 0.0)
        .collect()
}

/// Quebra um texto em termos comparáveis: minúsculas, sem pontuação, sem
/// stopwords e sem termos de 1 caractere.
fn tokenize(text: &str) -> Vec<String> {
    text.to_lowercase()
        .split(|c: char| !c.is_alphanumeric() && c != '_' && c != '-')
        .filter(|t| t.chars().count() > 1 && !STOPWORDS.contains(t))
        .map(str::to_string)
        .collect()
}

/// IDF simples por termo: termo presente em quase toda memória pesa pouco.
fn idf_table(memories: &[Memory]) -> HashMap<String, f32> {
    let total = memories.len() as f32;
    let mut docs: HashMap<String, f32> = HashMap::new();
    for m in memories {
        let mut seen: Vec<String> = tokenize(&format!("{} {}", m.title, m.body));
        seen.sort();
        seen.dedup();
        for t in seen {
            *docs.entry(t).or_insert(0.0) += 1.0;
        }
    }
    docs.into_iter()
        .map(|(t, n)| {
            let idf = ((total + 1.0) / (n + 1.0)).ln() + 1.0;
            (t, idf)
        })
        .collect()
}

/// Soma de IDF dos termos da consulta presentes na memória, normalizada
/// pelo tamanho da consulta. Título pesa o dobro do corpo.
fn overlap_score(terms: &[String], memory: &Memory, idf: &HashMap<String, f32>) -> f32 {
    if terms.is_empty() {
        return 0.0;
    }
    let title: Vec<String> = tokenize(&memory.title);
    let body: Vec<String> = tokenize(&memory.body);
    let mut total = 0.0;
    let mut matched = 0.0;
    for term in terms {
        let w = idf.get(term).copied().unwrap_or(1.0);
        total += w;
        if title.iter().any(|t| t == term) {
            matched += w * 2.0;
        } else if body.iter().any(|t| t == term) {
            matched += w;
        }
    }
    if total == 0.0 {
        0.0
    } else {
        (matched / total).min(2.0)
    }
}

/// Resumo de uma linha para o índice: primeira linha útil do corpo, cortada.
pub fn summarize(memory: &Memory, max_chars: usize) -> String {
    let flat = memory
        .body
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect::<Vec<_>>()
        .join(" · ");
    if flat.chars().count() <= max_chars {
        return flat;
    }
    let cut: String = flat.chars().take(max_chars.saturating_sub(1)).collect();
    format!("{cut}…")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mem(kind: MemoryKind, title: &str, body: &str, priority: i64) -> Memory {
        crate::test_memory(kind, title, body, priority)
    }

    #[test]
    fn ranks_by_term_overlap_with_the_question() {
        let memories = vec![
            mem(MemoryKind::Syntax, "estilo de commit", "mensagens em português", 0),
            mem(MemoryKind::Architecture, "banco de dados", "usamos sqlite com rusqlite", 0),
            mem(MemoryKind::Syntax, "testes", "cargo test no workspace", 0),
        ];
        let top = rank("como conecto no sqlite do banco?", memories, 2);
        assert_eq!(top[0].memory.title, "banco de dados");
        assert!(top[0].score > top[1].score);
    }

    #[test]
    fn security_rules_survive_even_without_matching_words() {
        let memories = vec![
            mem(MemoryKind::Security, "nunca rodar rm -rf", "deny-regex: rm\\s+-rf", 10),
            mem(MemoryKind::Syntax, "cores do tema", "usar azul", 0),
            mem(MemoryKind::Syntax, "fontes", "usar mono", 0),
        ];
        // Pergunta que não menciona segurança nenhuma.
        let top = rank("qual a fonte do tema?", memories, 2);
        assert!(
            top.iter().any(|r| r.memory.kind == MemoryKind::Security),
            "as regras do dono precisam aparecer sempre: {top:?}"
        );
    }

    #[test]
    fn respects_top_k_and_is_deterministic() {
        let memories: Vec<Memory> = (0..10)
            .map(|i| mem(MemoryKind::Syntax, &format!("nota {i}"), "corpo", 0))
            .collect();
        let a = rank("nota", memories.clone(), 4);
        let b = rank("nota", memories, 4);
        assert_eq!(a.len(), 4);
        let titles = |v: &Vec<Relevant>| {
            v.iter().map(|r| r.memory.title.clone()).collect::<Vec<_>>()
        };
        assert_eq!(titles(&a), titles(&b));
    }

    #[test]
    fn priority_breaks_ties_between_similar_memories() {
        let memories = vec![
            mem(MemoryKind::Syntax, "deploy comum", "deploy pelo script", 0),
            mem(MemoryKind::Syntax, "deploy importante", "deploy pelo script", 5),
        ];
        let top = rank("deploy", memories, 1);
        assert_eq!(top[0].memory.title, "deploy importante");
    }

    #[test]
    fn stopwords_and_short_words_do_not_drive_the_ranking() {
        let memories = vec![
            mem(MemoryKind::Syntax, "de para e o a", "de para e o a", 0),
            mem(MemoryKind::Syntax, "migrations", "rodar migrations antes do deploy", 0),
        ];
        let top = rank("preciso de migrations", memories, 1);
        assert_eq!(top[0].memory.title, "migrations");
    }

    #[test]
    fn the_owner_wins_over_an_agent_with_the_same_words() {
        let mut ia = mem(MemoryKind::Practice, "deploy da ia", "deploy manual", 0);
        ia.origin = crate::Origin::Agent;
        let dono = mem(MemoryKind::Practice, "deploy do dono", "deploy manual", 0);
        let top = rank("deploy manual", vec![ia, dono], 1);
        assert_eq!(top[0].memory.title, "deploy do dono");
    }

    #[test]
    fn only_what_relates_to_the_question_is_offered() {
        let memories = vec![
            mem(MemoryKind::Architecture, "banco de dados", "postgres", 0),
            mem(MemoryKind::Syntax, "cores do tema", "azul", 0),
            mem(MemoryKind::Security, "nunca rm", "deny-regex: rm", 5),
        ];
        let achados = rank_relevant("qual banco de dados?", memories, 5);
        let titulos: Vec<&str> = achados.iter().map(|r| r.memory.title.as_str()).collect();
        assert!(titulos.contains(&"banco de dados"), "{titulos:?}");
        assert!(!titulos.contains(&"cores do tema"), "sem relação não entra: {titulos:?}");
        assert!(titulos.contains(&"nunca rm"), "segurança entra pelo piso: {titulos:?}");
    }

    #[test]
    fn empty_inputs_are_safe() {
        assert!(rank("", Vec::new(), 4).is_empty());
        assert!(rank("qualquer coisa", Vec::new(), 4).is_empty());
        let memories = vec![mem(MemoryKind::Syntax, "x", "y", 0)];
        assert!(rank("q", memories, 0).is_empty());
    }

    #[test]
    fn summarize_flattens_and_truncates() {
        let m = mem(MemoryKind::Syntax, "t", "linha um\n\nlinha dois", 0);
        assert_eq!(summarize(&m, 100), "linha um · linha dois");
        let s = summarize(&m, 8);
        assert!(s.ends_with('…'));
        assert_eq!(s.chars().count(), 8);
    }
}
