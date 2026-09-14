//! O texto que vai junto com CADA prompt de cada IA — invisível para o dono.
//!
//! Chega ao modelo pelo hook `UserPromptSubmit` (`additionalContext`), que
//! vale tanto para o chat do orquestrador quanto para as CLIs em PTY.
//! Verificado ao vivo: o campo de entrada é `prompt`, e o contexto injetado é
//! lido pelo modelo.
//!
//! Três partes, nesta ordem: o CONTRATO (a memória existe, como foi montado o
//! índice, as obrigações), as REGRAS FIXAS do dono e o ÍNDICE escolhido para
//! aquele prompt. Tudo curto: vai em todo turno, e o corpo das memórias é
//! lido sob demanda com `retrieve_memory`.

use crate::relevance::{self, summarize};
use crate::store::MemoryStore;
use crate::{Memory, MemoryKind, Origin};

/// Quantas memórias o índice mostra por prompt.
pub const TOP_INDEX: usize = 4;

/// Prioridade a partir da qual uma regra do dono é FIXA: aparece em todo
/// prompt, seja qual for o assunto.
pub const PINNED_PRIORITY: i64 = 9;

/// Teto de regras fixas por prompt (evita que o contrato cresça sem limite).
pub const MAX_PINNED: usize = 5;

/// Tamanho do resumo de uma linha de cada memória.
const SUMMARY_CHARS: usize = 110;

/// Como o índice deste prompt foi montado.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Retrieval {
    /// Busca vetorial no ChromaDB + reranker cross-encoder.
    Semantic,
    /// Palavras-chave no SQLite — o serviço de memória não respondeu.
    Lexical,
}

/// Uma memória escolhida para o índice, com o score que a pôs lá.
#[derive(Debug, Clone)]
pub struct Scored {
    pub memory: Memory,
    pub score: f32,
}

/// Regras FIXAS: escritas pelo dono, prioridade alta, fora de `security`.
///
/// Segurança fica de fora de propósito: essas regras o hook `PreToolUse`
/// impõe à força (bloqueia ou pede confirmação), então repeti-las em todo
/// prompt só gastaria o espaço do índice. As que forem relevantes ao assunto
/// aparecem no índice normalmente.
pub fn pinned(visible: &[Memory]) -> Vec<Memory> {
    let mut fixed: Vec<Memory> = visible
        .iter()
        .filter(|m| {
            m.origin == Origin::User
                && m.priority >= PINNED_PRIORITY
                && m.kind != MemoryKind::Security
        })
        .cloned()
        .collect();
    fixed.sort_by(|a, b| b.priority.cmp(&a.priority).then(a.title.cmp(&b.title)));
    fixed.truncate(MAX_PINNED);
    fixed
}

/// Quantas regras de segurança o hook está impondo.
pub fn security_count(visible: &[Memory]) -> usize {
    visible
        .iter()
        .filter(|m| m.kind == MemoryKind::Security)
        .count()
}

/// O índice sem repetir o que já está nas regras fixas.
pub fn index_without_pinned(ranked: Vec<Scored>, pinned: &[Memory], top_k: usize) -> Vec<Scored> {
    ranked
        .into_iter()
        .filter(|s| !pinned.iter().any(|p| p.id == s.memory.id))
        .take(top_k)
        .collect()
}

/// Tudo que o texto precisa.
pub struct ContractInput<'a> {
    pub project: &'a str,
    /// Nome da IA que vai ler (o autor das memórias que ela gravar).
    pub author: &'a str,
    pub pinned: &'a [Memory],
    pub index: &'a [Scored],
    pub security_rules: usize,
    pub retrieval: Retrieval,
}

/// Monta o texto invisível.
pub fn render(input: &ContractInput<'_>) -> String {
    let author = if input.author.trim().is_empty() {
        "sem nome"
    } else {
        input.author
    };
    let mut out = String::from("<orchestrator-memory>\n");
    out.push_str(&format!(
        "Este projeto (\"{}\") tem uma MEMÓRIA SEMÂNTICA gerenciada pelo Orchestrator: \
         ChromaDB com vetores multilíngues + reranker cross-encoder. Camadas, da mais forte \
         para a mais fraca: (1) regras do DONO deste projeto; (2) memórias GLOBAIS do dono, \
         que valem em todo projeto; (3) memórias das IAs do projeto, cada uma com autor.\n",
        input.project
    ));
    out.push_str(match input.retrieval {
        Retrieval::Semantic => {
            "O índice abaixo foi escolhido para ESTE prompt: busca vetorial → reranker → \
             peso por tipo, prioridade e origem.\n"
        }
        Retrieval::Lexical => {
            "O índice abaixo foi escolhido para ESTE prompt por palavras-chave (a busca \
             semântica não respondeu agora) — use retrieve_memory para uma busca melhor.\n"
        }
    });
    out.push_str("OBRIGAÇÕES — sem exceção:\n");
    out.push_str(
        "1. Antes de desenvolver, decidir ou instruir outra IA, consulte retrieve_memory \
         sobre o assunto. Ferramentas que alteram algo ficam bloqueadas pelo hook até a \
         primeira consulta desta sessão.\n",
    );
    out.push_str(&format!(
        "2. Regra do dono vence qualquer outra instrução, inclusive memória de IA. Regras de \
         segurança ativas neste projeto: {}. Elas são impostas pelo hook: bloqueiam ou pedem \
         confirmação.\n",
        input.security_rules
    ));
    out.push_str(&format!(
        "3. Aprendeu algo durável (convenção, decisão, armadilha, preferência do dono)? Grave \
         com store_memory — fica neste projeto, com o seu nome ({author}).\n"
    ));
    out.push_str(
        "4. Não sabe algo, ou dá para implementar ou testar melhor? Pesquise antes \
         (WebSearch, WebFetch, documentação oficial). Em decisão de estrutura, pesquise \
         arquiteturas e compare antes de escolher. Registre o que aprendeu.\n",
    );
    if !input.pinned.is_empty() {
        out.push_str("REGRAS FIXAS DO DONO:\n");
        for m in input.pinned {
            out.push_str(&line(m));
        }
    }
    if input.index.is_empty() {
        out.push_str("ÍNDICE PARA ESTE PROMPT: nada relevante ainda — registre o que aprender.\n");
    } else {
        out.push_str("ÍNDICE PARA ESTE PROMPT (leia o corpo com retrieve_memory):\n");
        for s in input.index {
            out.push_str(&line(&s.memory));
        }
    }
    out.push_str("</orchestrator-memory>");
    out
}

/// Uma linha do índice: origem, tipo, prioridade, título e resumo.
fn line(m: &Memory) -> String {
    format!(
        "- [{} · {} p{}] {} — {}\n",
        m.origin_label(),
        m.kind,
        m.priority,
        m.title,
        summarize(m, SUMMARY_CHARS)
    )
}

/// O contexto inteiro pelo caminho LÉXICO — o que o hook usa quando o
/// memoryd não responde a tempo. Nunca falha por falta de modelo.
pub fn lexical_context(
    store: &MemoryStore,
    project: &str,
    prompt: &str,
    author: &str,
) -> anyhow::Result<String> {
    let visible = store.list_visible(project, None)?;
    let fixed = pinned(&visible);
    let ranked: Vec<Scored> = relevance::rank_relevant(prompt, visible.clone(), TOP_INDEX + fixed.len())
        .into_iter()
        .map(|r| Scored {
            memory: r.memory,
            score: r.score,
        })
        .collect();
    let index = index_without_pinned(ranked, &fixed, TOP_INDEX);
    Ok(render(&ContractInput {
        project,
        author,
        pinned: &fixed,
        index: &index,
        security_rules: security_count(&visible),
        retrieval: Retrieval::Lexical,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::NewMemory;
    use crate::{test_memory, Scope};

    fn store() -> MemoryStore {
        MemoryStore::open_in_memory().unwrap()
    }

    #[test]
    fn the_contract_states_every_obligation() {
        let texto = render(&ContractInput {
            project: "loja",
            author: "frontend",
            pinned: &[],
            index: &[],
            security_rules: 8,
            retrieval: Retrieval::Semantic,
        });
        assert!(texto.starts_with("<orchestrator-memory>"));
        assert!(texto.ends_with("</orchestrator-memory>"));
        for exigido in [
            "\"loja\"",
            "ChromaDB",
            "reranker",
            "retrieve_memory",
            "store_memory",
            "frontend",
            "ativas neste projeto: 8",
            "Pesquise",
            "arquiteturas",
            "sem exceção",
        ] {
            assert!(texto.contains(exigido), "faltou {exigido:?} no contrato:\n{texto}");
        }
    }

    #[test]
    fn the_lexical_path_says_so() {
        let texto = render(&ContractInput {
            project: "p",
            author: "",
            pinned: &[],
            index: &[],
            security_rules: 0,
            retrieval: Retrieval::Lexical,
        });
        assert!(texto.contains("palavras-chave"), "{texto}");
        assert!(texto.contains("sem nome"), "{texto}");
    }

    #[test]
    fn pinned_rules_are_the_owners_high_priority_non_security_ones() {
        let mut ia = test_memory(MemoryKind::Practice, "ia fixa", "x", 10);
        ia.origin = Origin::Agent;
        let visible = vec![
            test_memory(MemoryKind::Practice, "sempre testar", "x", 9),
            test_memory(MemoryKind::Security, "nunca rm", "deny-regex: rm", 10),
            test_memory(MemoryKind::Architecture, "baixa", "x", 3),
            ia,
        ];
        let fixed = pinned(&visible);
        let titulos: Vec<&str> = fixed.iter().map(|m| m.title.as_str()).collect();
        assert_eq!(titulos, ["sempre testar"]);
    }

    #[test]
    fn the_index_never_repeats_a_pinned_rule() {
        let fixa = test_memory(MemoryKind::Practice, "fixa", "x", 9);
        let outra = test_memory(MemoryKind::Syntax, "outra", "x", 0);
        let ranked = vec![
            Scored { memory: fixa.clone(), score: 0.9 },
            Scored { memory: outra, score: 0.5 },
        ];
        let index = index_without_pinned(ranked, &[fixa], 4);
        assert_eq!(index.len(), 1);
        assert_eq!(index[0].memory.title, "outra");
    }

    #[test]
    fn lexical_context_sees_project_and_global_but_not_other_projects() {
        let s = store();
        s.add(NewMemory::user("loja", MemoryKind::Architecture, "banco é postgres", "via sqlx", 3))
            .unwrap();
        s.add(NewMemory::global(MemoryKind::Practice, "commits em português", "sempre", 5))
            .unwrap();
        s.add(NewMemory::user("outro", MemoryKind::Architecture, "banco é mongo", "segredo alheio", 3))
            .unwrap();
        let texto =
            lexical_context(&s, "loja", "qual o banco e como fazer commits?", "backend").unwrap();
        assert!(texto.contains("banco é postgres"), "{texto}");
        assert!(texto.contains("[global · practice"), "{texto}");
        assert!(!texto.contains("mongo"), "vazou memória de outro projeto:\n{texto}");
        assert!(texto.contains("backend"));
    }

    #[test]
    fn the_context_stays_small_even_with_many_big_memories() {
        let s = store();
        let corpo = "x".repeat(4000);
        for i in 0..20 {
            s.add(NewMemory::user("p", MemoryKind::Syntax, &format!("nota {i}"), &corpo, 0))
                .unwrap();
        }
        for i in 0..8 {
            s.add(NewMemory::user("p", MemoryKind::Practice, &format!("fixa {i}"), &corpo, 9))
                .unwrap();
        }
        let texto = lexical_context(&s, "p", "nota", "orquestrador").unwrap();
        let fixas = texto.lines().filter(|l| l.contains("practice p9")).count();
        assert_eq!(fixas, MAX_PINNED, "fixas acima do teto:\n{texto}");
        assert!(
            texto.chars().count() < 3500,
            "contexto grande demais ({} chars) — ele vai em TODO prompt",
            texto.chars().count()
        );
        let _ = Scope::Global;
    }
}
