//! Regras de segurança que todo projeto ganha de fábrica.
//!
//! O gate do Orchestrator (hook `PreToolUse`) só age sobre o que está na
//! memória: sem nenhuma regra, ele libera tudo e NENHUMA decisão chega ao
//! usuário — o que na prática parece um gate quebrado. Em vez de exigir que
//! o dono cadastre regras na mão, semeamos um conjunto mínimo e sensato no
//! primeiro setup do projeto.
//!
//! `deny-regex:` bloqueia direto; `ask-regex:` pausa e pergunta ao usuário.
//! O dono continua livre para editar, apagar ou acrescentar (F2 → memórias,
//! ou `orchestrator memory add --kind security`).

use anyhow::Result;

use crate::store::MemoryStore;
use crate::MemoryKind;

/// Uma regra semeada: título, corpo (com as diretivas) e prioridade.
pub struct DefaultRule {
    pub title: &'static str,
    pub body: &'static str,
    pub priority: i64,
}

/// O conjunto padrão. Bloqueia o que destrói máquina/dados sem volta e
/// pergunta no que é arriscado mas legítimo no dia a dia.
pub const DEFAULT_SECURITY_RULES: &[DefaultRule] = &[
    DefaultRule {
        title: "não apagar árvores de arquivos em massa",
        body: "Remoção recursiva e forçada não tem volta.\n\
               deny-regex: \\brm\\s+-[a-zA-Z]*[rR][a-zA-Z]*f\\b|\\brm\\s+-[a-zA-Z]*f[a-zA-Z]*[rR]\\b",
        priority: 10,
    },
    DefaultRule {
        title: "não formatar nem escrever em dispositivos",
        body: "Formatar disco ou escrever em /dev destrói a máquina.\n\
               deny-regex: \\bmkfs|\\bdd\\s+if=.*of=/dev/|>\\s*/dev/sd",
        priority: 10,
    },
    DefaultRule {
        title: "não apagar bancos de dados",
        body: "Perda de dados irreversível.\n\
               deny-regex: \\bDROP\\s+DATABASE\\b|\\bTRUNCATE\\s+TABLE\\b",
        priority: 10,
    },
    DefaultRule {
        title: "confirmar reescrita de histórico do git",
        body: "Force-push e reset --hard descartam trabalho de alguém.\n\
               ask-regex: \\bgit\\s+push\\s+.*(--force|-f)\\b|\\bgit\\s+reset\\s+--hard\\b|\\bgit\\s+clean\\s+-[a-zA-Z]*f\\b",
        priority: 8,
    },
    DefaultRule {
        title: "confirmar migração e alteração de esquema",
        body: "Alterar esquema em banco existente pede decisão do dono.\n\
               ask-regex: \\bDROP\\s+TABLE\\b|\\bALTER\\s+TABLE\\b|\\bmigrate\\b|\\bmigrations?\\b",
        priority: 7,
    },
    DefaultRule {
        title: "confirmar publicação e deploy",
        body: "Publicar afeta gente de fora do projeto.\n\
               ask-regex: \\bnpm\\s+publish\\b|\\bcargo\\s+publish\\b|\\bdocker\\s+push\\b|\\bkubectl\\s+apply\\b|\\bterraform\\s+apply\\b|\\bgh\\s+release\\s+create\\b",
        priority: 8,
    },
    DefaultRule {
        title: "confirmar mexida em segredos e credenciais",
        body: "Arquivos de segredo não devem ser lidos, movidos ou enviados sem o dono saber.\n\
               ask-regex: \\.env\\b|\\bid_rsa\\b|\\bcredentials\\b|\\bsecrets?\\.(json|ya?ml|toml)\\b|\\.pem\\b",
        priority: 9,
    },
    DefaultRule {
        title: "confirmar permissões abertas demais",
        body: "chmod 777 e afins deixam o sistema exposto.\n\
               ask-regex: \\bchmod\\s+(-R\\s+)?777\\b|\\bchown\\s+-R\\s+root\\b",
        priority: 7,
    },
];

/// Marca no `ui_state` que o projeto já recebeu as regras padrão — assim,
/// se o dono apagar alguma de propósito, ela não volta no próximo setup.
fn seeded_key(project: &str) -> String {
    format!("security.seeded.{project}")
}

/// Semeia as regras padrão no projeto, uma única vez.
///
/// Devolve quantas regras foram criadas (0 quando o projeto já tinha regras
/// ou já foi semeado antes). Idempotente e seguro para chamar a cada setup.
pub fn seed_default_security_rules(store: &MemoryStore, project: &str) -> Result<usize> {
    if store.ui_get(&seeded_key(project))?.is_some() {
        return Ok(0);
    }
    // Projeto que já tem regra própria não é tocado.
    if has_gate_rules(store, project)? {
        store.ui_set(&seeded_key(project), "preexisting")?;
        return Ok(0);
    }
    let mut created = 0;
    for rule in DEFAULT_SECURITY_RULES {
        store.add_memory(
            project,
            MemoryKind::Security,
            rule.title,
            rule.body,
            rule.priority,
        )?;
        created += 1;
    }
    store.ui_set(&seeded_key(project), "seeded")?;
    Ok(created)
}

/// O projeto tem alguma regra que o gate saiba aplicar?
pub fn has_gate_rules(store: &MemoryStore, project: &str) -> Result<bool> {
    let memories = store.list(project, Some(MemoryKind::Security))?;
    Ok(memories
        .iter()
        .any(|m| m.body.contains("deny-regex:") || m.body.contains("ask-regex:")))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> MemoryStore {
        MemoryStore::open_in_memory().unwrap()
    }

    #[test]
    fn seeds_once_and_makes_the_gate_active() {
        let s = store();
        assert!(!has_gate_rules(&s, "p").unwrap());
        let n = seed_default_security_rules(&s, "p").unwrap();
        assert_eq!(n, DEFAULT_SECURITY_RULES.len());
        assert!(has_gate_rules(&s, "p").unwrap());
        // Segunda chamada não duplica.
        assert_eq!(seed_default_security_rules(&s, "p").unwrap(), 0);
        assert_eq!(
            s.list("p", Some(MemoryKind::Security)).unwrap().len(),
            DEFAULT_SECURITY_RULES.len()
        );
    }

    #[test]
    fn does_not_touch_a_project_that_already_has_rules() {
        let s = store();
        s.add_memory(
            "p",
            MemoryKind::Security,
            "minha regra",
            "ask-regex: deploy",
            5,
        )
        .unwrap();
        assert_eq!(seed_default_security_rules(&s, "p").unwrap(), 0);
        assert_eq!(s.list("p", Some(MemoryKind::Security)).unwrap().len(), 1);
    }

    #[test]
    fn deleting_a_seeded_rule_does_not_bring_it_back() {
        let s = store();
        seed_default_security_rules(&s, "p").unwrap();
        let all = s.list("p", Some(MemoryKind::Security)).unwrap();
        s.delete(&all[0].id).unwrap();
        let before = s.list("p", Some(MemoryKind::Security)).unwrap().len();
        assert_eq!(seed_default_security_rules(&s, "p").unwrap(), 0);
        assert_eq!(s.list("p", Some(MemoryKind::Security)).unwrap().len(), before);
    }

    #[test]
    fn projects_are_seeded_independently() {
        let s = store();
        seed_default_security_rules(&s, "a").unwrap();
        assert!(!has_gate_rules(&s, "b").unwrap());
        assert!(seed_default_security_rules(&s, "b").unwrap() > 0);
    }

    #[test]
    fn every_default_rule_carries_a_directive_the_gate_understands() {
        for rule in DEFAULT_SECURITY_RULES {
            assert!(
                rule.body.contains("deny-regex:") || rule.body.contains("ask-regex:"),
                "regra sem diretiva: {}",
                rule.title
            );
        }
    }
}
