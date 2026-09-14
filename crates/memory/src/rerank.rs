//! Pesos comuns aos dois caminhos de busca — o semântico, no memoryd, e o
//! léxico, em [`crate::relevance`]: o que torna uma memória mais importante
//! que outra de relevância parecida. Funções puras, sem I/O.

use crate::{Memory, MemoryKind, Origin};

/// Similaridade de cosseno entre dois vetores (0.0 se as dimensões diferem
/// ou algum vetor é nulo).
pub fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    let dot: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
    let na: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let nb: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    if na == 0.0 || nb == 0.0 {
        0.0
    } else {
        dot / (na * nb)
    }
}

/// Peso por tipo de memória — `security` sempre vem primeiro em empates, e
/// as práticas do dono logo depois.
pub fn kind_weight(kind: MemoryKind) -> f32 {
    match kind {
        MemoryKind::Security => 1.5,
        MemoryKind::Practice => 1.3,
        MemoryKind::Architecture => 1.2,
        MemoryKind::Decision => 1.1,
        MemoryKind::Syntax => 1.0,
    }
}

/// Boost multiplicativo por prioridade (10% por nível, limitado a 2x).
pub fn priority_boost(priority: i64) -> f32 {
    let p = priority.max(0) as f32;
    (1.0 + 0.1 * p).min(2.0)
}

/// Regra do dono pesa mais que nota de IA com relevância parecida.
pub fn origin_weight(origin: Origin) -> f32 {
    match origin {
        Origin::User => 1.0,
        Origin::Agent => 0.8,
    }
}

/// Converte o score bruto do cross-encoder (um logit, que pode ser negativo)
/// para a faixa 0..1, comparável com a relevância léxica.
pub fn cross_score(logit: f32) -> f32 {
    1.0 / (1.0 + (-logit).exp())
}

/// Score final de uma memória, dada a relevância (0..1) para o prompt.
pub fn weighted(relevance: f32, memory: &Memory) -> f32 {
    relevance
        * kind_weight(memory.kind)
        * priority_boost(memory.priority)
        * origin_weight(memory.origin)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_memory;

    #[test]
    fn cosine_basics() {
        assert!((cosine_similarity(&[1.0, 0.0], &[1.0, 0.0]) - 1.0).abs() < 1e-6);
        assert!(cosine_similarity(&[1.0, 0.0], &[0.0, 1.0]).abs() < 1e-6);
        assert_eq!(cosine_similarity(&[1.0], &[1.0, 2.0]), 0.0);
        assert_eq!(cosine_similarity(&[0.0, 0.0], &[1.0, 1.0]), 0.0);
    }

    #[test]
    fn security_outranks_syntax_at_equal_relevance() {
        let sec = test_memory(MemoryKind::Security, "a", "b", 0);
        let syn = test_memory(MemoryKind::Syntax, "a", "b", 0);
        assert!(weighted(0.8, &sec) > weighted(0.8, &syn));
    }

    #[test]
    fn priority_boosts_and_is_capped() {
        let low = test_memory(MemoryKind::Syntax, "a", "b", 0);
        let high = test_memory(MemoryKind::Syntax, "a", "b", 5);
        assert!(weighted(0.8, &high) > weighted(0.8, &low));
        assert_eq!(priority_boost(100), 2.0);
        assert_eq!(priority_boost(-3), 1.0);
    }

    #[test]
    fn the_owner_outranks_an_agent_saying_the_same_thing() {
        let dono = test_memory(MemoryKind::Practice, "a", "b", 0);
        let mut ia = dono.clone();
        ia.origin = Origin::Agent;
        assert!(weighted(0.7, &dono) > weighted(0.7, &ia));
    }

    #[test]
    fn cross_encoder_scores_become_comparable() {
        assert!((cross_score(0.0) - 0.5).abs() < 1e-6);
        assert!(cross_score(6.0) > 0.99);
        assert!(cross_score(-6.0) < 0.01);
        assert!(cross_score(1.0) > cross_score(-1.0));
    }
}
