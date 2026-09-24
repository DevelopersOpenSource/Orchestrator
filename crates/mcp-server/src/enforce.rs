//! Avaliação de regras de `security` da memória contra uma tool call.
//!
//! Regras são memórias de tipo `security` cujo corpo pode conter linhas:
//! - `deny-regex: <regex>` — bloqueia imediatamente a tool call;
//! - `ask-regex: <regex>` — pausa e pede decisão. No modo autônomo, quando
//!   quem pede é uma CLI do workspace, quem decide é o orquestrador (e ele
//!   escala ao dono o que for crítico demais ou não bater com o pedido);
//! - `owner-regex: <regex>` — pausa e pede decisão SEMPRE ao dono, em
//!   qualquer modo: para o que ele quer decidir pessoalmente.
//!
//! Os regexes são testados contra o texto serializado da tool call
//! (nome + input JSON) E contra uma versão NORMALIZADA dele — ver
//! [`normalize`]. Sem isso o gate cai em truques triviais de shell: aspas no
//! meio da palavra (`r''m -r''f`), barra invertida (`r\m`) ou espaço
//! repetido produzem o mesmo comando e escapariam de `rm\s+-rf`.
//!
//! Regras sem essas linhas são apenas contexto e nunca bloqueiam nada.

use orchestrator_memory::store::MemoryStore;
use orchestrator_memory::MemoryKind;
use regex::Regex;

/// Veredito do enforcement para uma tool call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// Nenhuma regra casou: segue o fluxo normal de permissões.
    Allow,
    /// Uma regra `deny-regex` casou: bloquear com o motivo dado.
    Deny { rule_title: String },
    /// Uma regra `ask-regex` (ou `owner-regex`, com `owner_only`) casou:
    /// pausar e pedir decisão.
    Ask { rule_title: String, owner_only: bool },
}

/// Campos que carregam TEXTO LIVRE (não um comando) nas tools do
/// orquestrador: instruções para uma CLI, texto digitado num formulário, o
/// motivo de uma decisão, uma pergunta ao dono.
const CONTENT_FIELDS: &[&str] = &[
    "prompt",
    "text",
    "value",
    "search",
    "description",
    "reason",
    "question",
    "options",
    "answer",
];

/// O que de fato vale avaliar numa tool call.
///
/// Para as tools do orquestrador (`cli_*`, `ui_*`), o corpo do prompt é
/// CONTEÚDO, não comando: mandar uma CLI "não use rm -rf" não é executar
/// `rm -rf`, e bloquear isso trava o trabalho por uma menção. O que a CLI
/// realmente executar passa pelo gate DELA — a defesa fica na execução, onde
/// ela vale. Os campos estruturais (nome, comando, url) continuam avaliados.
pub fn actionable_input(tool_name: &str, tool_input: &str) -> String {
    if !tool_name.starts_with("mcp__orchestrator__") {
        return tool_input.to_string();
    }
    let Ok(value) = serde_json::from_str::<serde_json::Value>(tool_input) else {
        return tool_input.to_string();
    };
    let Some(obj) = value.as_object() else {
        return tool_input.to_string();
    };
    let kept: serde_json::Map<String, serde_json::Value> = obj
        .iter()
        .filter(|(k, _)| !CONTENT_FIELDS.contains(&k.as_str()))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    serde_json::Value::Object(kept).to_string()
}

/// Reduz um comando ao que o shell de fato executaria, para o casamento de
/// regras não depender de como ele foi escrito.
///
/// Remove aspas e barras invertidas (que só servem para quebrar a palavra) e
/// colapsa espaços em branco. `r''m   -r''f  /x` vira `rm -rf /x`, então
/// uma regra `rm\s+-rf` volta a pegar. É deliberadamente conservador: só
/// mexe em pontuação de quoting, nunca no conteúdo.
pub fn normalize(command: &str) -> String {
    let stripped: String = command
        .chars()
        .filter(|c| !matches!(c, '\'' | '"' | '\\' | '`'))
        .collect();
    stripped.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Extrai os padrões `deny-regex:`/`ask-regex:` do corpo de uma regra.
fn patterns_of(body: &str, prefix: &str) -> Vec<Regex> {
    body.lines()
        .filter_map(|l| l.trim().strip_prefix(prefix))
        .filter_map(|p| Regex::new(p.trim()).ok())
        .collect()
}

/// Avalia as regras `security` do projeto contra a tool call dada.
///
/// `deny` tem precedência sobre `ask`. Regras de maior prioridade são
/// avaliadas primeiro (o desempate entre veredictos é irrelevante:
/// qualquer `deny` vence).
pub fn evaluate(store: &MemoryStore, project: &str, tool_name: &str, tool_input: &str) -> Verdict {
    let raw = format!("{tool_name} {}", actionable_input(tool_name, tool_input));
    // Duas leituras do mesmo comando: a literal e a que o shell realmente
    // executa. Uma regra que casa qualquer uma delas vale.
    let normalized = normalize(&raw);
    let haystacks = [raw, normalized];
    let mut rules = match store.list_visible(project, Some(MemoryKind::Security)) {
        Ok(r) => r,
        Err(_) => return Verdict::Allow,
    };
    rules.sort_by_key(|r| -r.priority);

    // (título, só do dono). Regra do dono vence regra comum; deny vence tudo.
    let mut ask: Option<(String, bool)> = None;
    for rule in &rules {
        for re in patterns_of(&rule.body, "deny-regex:") {
            if haystacks.iter().any(|h| re.is_match(h)) {
                return Verdict::Deny {
                    rule_title: rule.title.clone(),
                };
            }
        }
        if !matches!(ask, Some((_, true)))
            && patterns_of(&rule.body, "owner-regex:")
                .iter()
                .any(|re| haystacks.iter().any(|h| re.is_match(h)))
        {
            ask = Some((rule.title.clone(), true));
        }
        if ask.is_none()
            && patterns_of(&rule.body, "ask-regex:")
                .iter()
                .any(|re| haystacks.iter().any(|h| re.is_match(h)))
        {
            ask = Some((rule.title.clone(), false));
        }
    }
    if ask.is_none() {
        if let Some(fora) = fora_da_pasta(store, project, tool_name, tool_input) {
            ask = Some((format!("acesso fora da pasta do projeto: {fora}"), true));
        }
    }
    match ask {
        Some((rule_title, owner_only)) => Verdict::Ask {
            rule_title,
            owner_only,
        },
        None => Verdict::Allow,
    }
}

/// Chave em `ui_state` com as pastas que o dono liberou para um projeto
/// (uma por linha), além da própria pasta do projeto.
pub fn allowed_dirs_key(project: &str) -> String {
    format!("pastas.autorizadas.{project}")
}

/// Caminhos que um comando pode citar sem ser "fora do projeto": binários
/// do sistema, descarte de saída, temporários.
const LIVRES: &[&str] = &[
    "/dev/null", "/dev/stdout", "/dev/stderr", "/dev/stdin", "/dev/tty", "/tmp", "/usr", "/bin",
    "/sbin", "/lib", "/lib64", "/proc/self",
];

/// Resolve `.`/`..` sem tocar no disco (o caminho pode nem existir ainda).
fn normalizar(p: &std::path::Path) -> std::path::PathBuf {
    use std::path::Component;
    let mut out = std::path::PathBuf::new();
    for c in p.components() {
        match c {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// O primeiro caminho citado pela tool que cai FORA da pasta do projeto
/// (`ORCHESTRATOR_WORKDIR`) e das pastas que o dono autorizou.
///
/// Sem `ORCHESTRATOR_WORKDIR` (uso fora do Orchestrator) não há confinamento.
/// Arquivo por tool (Read/Write/Edit/Glob/Grep) é exato; no Bash é
/// heurístico — pega caminhos absolutos, `~/...` e `../...` citados no
/// comando, não um `cd` feito por variável.
pub fn fora_da_pasta(store: &MemoryStore, project: &str, tool_name: &str, tool_input: &str) -> Option<String> {
    let raiz = std::path::PathBuf::from(std::env::var_os("ORCHESTRATOR_WORKDIR")?);
    let extra = store.ui_get(&allowed_dirs_key(project)).ok().flatten().unwrap_or_default();
    caminho_fora(&raiz, &extra, tool_name, tool_input)
}

/// O miolo de [`fora_da_pasta`], sem ler ambiente nem banco (testável).
fn caminho_fora(raiz: &std::path::Path, extra: &str, tool_name: &str, tool_input: &str) -> Option<String> {
    use std::path::{Path, PathBuf};
    let mut permitidas = vec![normalizar(raiz)];
    if let Ok(c) = raiz.canonicalize() {
        permitidas.push(c);
    }
    permitidas.extend(extra.lines().filter(|l| !l.trim().is_empty()).map(|l| normalizar(Path::new(l.trim()))));
    let input: serde_json::Value = serde_json::from_str(tool_input).ok()?;
    let campo = |k: &str| input.get(k).and_then(|v| v.as_str()).map(str::to_string);
    let candidatos: Vec<String> = match tool_name {
        "Read" | "Write" | "Edit" | "MultiEdit" => campo("file_path").into_iter().collect(),
        "NotebookEdit" => campo("notebook_path").into_iter().collect(),
        "Glob" | "Grep" | "LS" => campo("path").into_iter().collect(),
        "Bash" => campo("command")
            .map(|c| {
                c.split(|ch: char| ch.is_whitespace() || ";|&<>()'\"=`".contains(ch))
                    .filter(|t| t.starts_with('/') || t.starts_with("~/") || t == &"~" || t.starts_with("../"))
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default(),
        _ => Vec::new(),
    };
    let home = std::env::var_os("HOME").map(PathBuf::from);
    for c in candidatos {
        let bruto = match (c.strip_prefix('~'), &home) {
            (Some(rest), Some(h)) => h.join(rest.trim_start_matches('/')),
            _ if c.starts_with("../") => raiz.join(&c),
            _ => PathBuf::from(&c),
        };
        let p = normalizar(&bruto);
        // No Bash, "/api/users" num grep não é caminho: só conta o que começa
        // numa pasta que existe de verdade na raiz (/home, /etc, /var...).
        if tool_name == "Bash" && c.starts_with('/') {
            let topo: PathBuf = p.components().take(2).collect();
            if !topo.exists() {
                continue;
            }
        }
        let livre = LIVRES.iter().any(|l| p == Path::new(l) || p.starts_with(l));
        if !livre && !permitidas.iter().any(|r| p.starts_with(r)) {
            return Some(p.display().to_string());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    #[test]
    fn files_outside_the_project_need_the_owner() {
        let raiz = std::path::Path::new("/home/eu/projeto");
        let fora = |tool: &str, input: &str| super::caminho_fora(raiz, "", tool, input);
        assert_eq!(fora("Read", r#"{"file_path":"/home/eu/projeto/src/a.rs"}"#), None);
        assert_eq!(fora("Edit", r#"{"file_path":"/home/eu/projeto/../outro/b.rs"}"#).as_deref(), Some("/home/eu/outro/b.rs"));
        assert!(fora("Read", r#"{"file_path":"/etc/passwd"}"#).is_some());
        // Bash: binário do sistema e /dev/null são livres; texto que só
        // parece caminho ("/api/users") não conta.
        assert_eq!(fora("Bash", r#"{"command":"/usr/bin/env node x.js > /dev/null"}"#), None);
        assert_eq!(fora("Bash", r#"{"command":"grep -r /api/users src"}"#), None);
        assert!(fora("Bash", r#"{"command":"cat /etc/hosts"}"#).is_some());
        assert!(fora("Bash", r#"{"command":"cd ../.. && ls"}"#).is_some());
        // Pasta liberada pelo dono deixa de pedir.
        assert_eq!(super::caminho_fora(raiz, "/etc\n", "Read", r#"{"file_path":"/etc/hosts"}"#), None);
    }

    use super::*;

    fn store_with_rules() -> MemoryStore {
        let s = MemoryStore::open_in_memory().unwrap();
        s.add_memory(
            "p",
            MemoryKind::Security,
            "nunca rm -rf",
            "Comandos destrutivos são proibidos.\ndeny-regex: rm\\s+-rf",
            10,
        )
        .unwrap();
        s.add_memory(
            "p",
            MemoryKind::Security,
            "migrações de banco pedem aprovação",
            "ask-regex: (?i)drop\\s+table|migrate",
            5,
        )
        .unwrap();
        s.add_memory(
            "p",
            MemoryKind::Security,
            "regra só de contexto",
            "Nunca commitar segredos (sem regex, não bloqueia nada).",
            0,
        )
        .unwrap();
        s
    }

    #[test]
    fn deny_matches_destructive_command() {
        let s = store_with_rules();
        let v = evaluate(&s, "p", "Bash", r#"{"command":"rm -rf /tmp/x"}"#);
        assert_eq!(
            v,
            Verdict::Deny {
                rule_title: "nunca rm -rf".into()
            }
        );
    }

    #[test]
    fn ask_matches_migration() {
        let s = store_with_rules();
        let v = evaluate(&s, "p", "Bash", r#"{"command":"sqlx migrate run"}"#);
        assert!(matches!(v, Verdict::Ask { .. }));
    }

    #[test]
    fn deny_beats_ask() {
        let s = store_with_rules();
        let v = evaluate(&s, "p", "Bash", r#"{"command":"rm -rf db && migrate"}"#);
        assert!(matches!(v, Verdict::Deny { .. }));
    }

    #[test]
    fn owner_rule_is_marked_and_beats_a_common_ask() {
        let s = store_with_rules();
        s.add_memory(
            "p",
            MemoryKind::Security,
            "produção é comigo",
            "owner-regex: (?i)--env\\s+prod",
            1,
        )
        .unwrap();
        // Casa a regra comum (migrate) e a do dono (--env prod): vale a do dono.
        let v = evaluate(&s, "p", "Bash", r#"{"command":"sqlx migrate run --env prod"}"#);
        assert_eq!(
            v,
            Verdict::Ask {
                rule_title: "produção é comigo".into(),
                owner_only: true
            }
        );
        let v = evaluate(&s, "p", "Bash", r#"{"command":"sqlx migrate run"}"#);
        assert!(matches!(v, Verdict::Ask { owner_only: false, .. }));
    }

    #[test]
    fn the_reason_of_a_decision_is_content_not_a_command() {
        let s = store_with_rules();
        let v = evaluate(
            &s,
            "p",
            "mcp__orchestrator__decision_resolve",
            r#"{"id":"abc","approve":true,"reason":"a migrate foi pedida pelo dono"}"#,
        );
        assert_eq!(v, Verdict::Allow);
    }

    #[test]
    fn clean_input_allows() {
        let s = store_with_rules();
        assert_eq!(
            evaluate(&s, "p", "Read", r#"{"file_path":"/src/main.rs"}"#),
            Verdict::Allow
        );
        // projeto sem regras também permite
        assert_eq!(evaluate(&s, "outro", "Bash", "rm -rf /"), Verdict::Allow);
    }
}

/// As regras semeadas de fábrica precisam funcionar de verdade no gate —
/// senão o usuário continua com um gate que nunca levanta nada.
#[cfg(test)]
mod default_rules_tests {
    use super::*;
    use orchestrator_memory::defaults::seed_default_security_rules;
    use orchestrator_memory::store::MemoryStore;

    fn seeded_store() -> MemoryStore {
        let s = MemoryStore::open_in_memory().unwrap();
        seed_default_security_rules(&s, "p").unwrap();
        s
    }

    fn verdict(s: &MemoryStore, command: &str) -> Verdict {
        evaluate(s, "p", "Bash", &format!("{{\"command\":\"{command}\"}}"))
    }

    #[test]
    fn destructive_commands_are_denied_out_of_the_box() {
        let s = seeded_store();
        for cmd in [
            "rm -rf /tmp/x",
            "rm -fr algo",
            "sudo mkfs.ext4 /dev/sda1",
            "dd if=/dev/zero of=/dev/sda",
        ] {
            assert!(
                matches!(verdict(&s, cmd), Verdict::Deny { .. }),
                "deveria bloquear: {cmd}"
            );
        }
    }

    #[test]
    fn risky_but_legitimate_commands_ask_the_owner() {
        let s = seeded_store();
        for cmd in [
            "git push --force origin main",
            "git reset --hard HEAD~3",
            "npm publish",
            "kubectl apply -f deploy.yaml",
            "chmod 777 /srv",
            "cat .env",
        ] {
            assert!(
                matches!(verdict(&s, cmd), Verdict::Ask { .. }),
                "deveria perguntar: {cmd}"
            );
        }
    }

    #[test]
    fn everyday_commands_stay_out_of_the_way() {
        let s = seeded_store();
        for cmd in [
            "cargo test",
            "ls -la",
            "git status",
            "git commit -m ok",
            "rm arquivo.txt",
            "npm run build",
        ] {
            assert!(
                matches!(verdict(&s, cmd), Verdict::Allow),
                "não deveria atrapalhar: {cmd}"
            );
        }
    }

    #[test]
    fn default_rules_do_not_fire_on_words_that_merely_contain_them() {
        // Sem fronteira de palavra, "warm -rf" e "confirm -rf" cairiam na
        // regra de remoção; "immigrate" na de migração.
        let s = seeded_store();
        for cmd in [
            "echo warm -rfc",
            "echo confirm -rf",
            "echo immigrate agora",
            "echo chmod 7777",
        ] {
            assert!(
                matches!(verdict(&s, cmd), Verdict::Allow),
                "falso positivo em: {cmd}"
            );
        }
        // E o comando de verdade continua pego.
        assert!(matches!(verdict(&s, "rm -rf /tmp/x"), Verdict::Deny { .. }));
        assert!(matches!(verdict(&s, "chmod 777 /srv"), Verdict::Ask { .. }));
    }

    #[test]
    fn deny_wins_over_ask_when_both_match() {
        let s = seeded_store();
        // Toca em segredo (ask) E apaga em massa (deny).
        assert!(matches!(
            verdict(&s, "rm -rf .env"),
            Verdict::Deny { .. }
        ));
    }
}

/// O gate não pode cair em truque de aspas: o usuário testou exatamente
/// isso ao vivo (`r''m -r''f`) e passou.
#[cfg(test)]
mod bypass_tests {
    use super::*;
    use orchestrator_memory::defaults::seed_default_security_rules;
    use orchestrator_memory::MemoryKind;

    fn store_with(rule_body: &str) -> MemoryStore {
        let s = MemoryStore::open_in_memory().unwrap();
        s.add_memory("p", MemoryKind::Security, "regra", rule_body, 5)
            .unwrap();
        s
    }

    fn verdict(s: &MemoryStore, command: &str) -> Verdict {
        evaluate(s, "p", "Bash", &format!("{{\"command\":\"{command}\"}}"))
    }

    #[test]
    fn normalize_undoes_shell_word_splitting_tricks() {
        assert_eq!(normalize("r''m -r''f /tmp/x"), "rm -rf /tmp/x");
        assert_eq!(normalize("r\\m -rf"), "rm -rf");
        assert_eq!(normalize("rm    -rf     /x"), "rm -rf /x");
        assert_eq!(normalize("\"rm\" -rf"), "rm -rf");
        // Conteúdo em si não é alterado.
        assert_eq!(normalize("echo ola mundo"), "echo ola mundo");
    }

    #[test]
    fn quoted_command_still_matches_the_rule() {
        let s = store_with("ask-regex: rm -r");
        // Forma literal.
        assert!(matches!(verdict(&s, "rm -rf /tmp/x"), Verdict::Ask { .. }));
        // Mesma coisa, escrita para escapar da regex.
        assert!(
            matches!(verdict(&s, "r''m -r''f /tmp/x"), Verdict::Ask { .. }),
            "aspas no meio da palavra não podem furar o gate"
        );
        assert!(matches!(
            verdict(&s, "r\\m -r\\f /tmp/x"),
            Verdict::Ask { .. }
        ));
    }

    #[test]
    fn default_rules_also_resist_the_trick() {
        let s = MemoryStore::open_in_memory().unwrap();
        seed_default_security_rules(&s, "p").unwrap();
        assert!(matches!(
            verdict(&s, "r''m -r''f /tmp/x"),
            Verdict::Deny { .. }
        ));
    }

    #[test]
    fn normalization_does_not_change_the_verdict_of_clean_commands() {
        let s = store_with("deny-regex: rm -rf");
        for cmd in ["cargo test", "ls -la", "git status", "echo ola mundo"] {
            assert!(
                matches!(verdict(&s, cmd), Verdict::Allow),
                "comando limpo não pode ser bloqueado: {cmd}"
            );
        }
    }

    #[test]
    fn loose_rules_match_substrings_with_or_without_normalization() {
        // `rm -rf` sem fronteira de palavra casa dentro de "warm -rfc" — isso
        // vem da regra escrita pelo dono, não da normalização (o veredito é o
        // mesmo nas duas leituras). Uma regra com `\b` resolve.
        let solta = store_with("deny-regex: rm -rf");
        assert!(matches!(verdict(&solta, "echo warm -rfc"), Verdict::Deny { .. }));

        let precisa = store_with("deny-regex: \\brm\\s+-rf");
        assert!(matches!(verdict(&precisa, "echo warm -rfc"), Verdict::Allow));
        // E continua pegando o comando de verdade, inclusive disfarçado.
        assert!(matches!(verdict(&precisa, "rm -rf /tmp/x"), Verdict::Deny { .. }));
        assert!(matches!(
            verdict(&precisa, "r\'\'m -r\'\'f /tmp/x"),
            Verdict::Deny { .. }
        ));
    }
}

/// O prompt que o orquestrador manda para uma CLI é conteúdo, não comando.
#[cfg(test)]
mod content_field_tests {
    use super::*;
    use orchestrator_memory::defaults::seed_default_security_rules;

    fn seeded() -> MemoryStore {
        let s = MemoryStore::open_in_memory().unwrap();
        seed_default_security_rules(&s, "p").unwrap();
        s
    }

    #[test]
    fn mentioning_a_dangerous_command_in_a_prompt_does_not_block_it() {
        let s = seeded();
        // Caso real: o orquestrador instruindo uma CLI, citando o comando.
        let input = r#"{"name":"backend","prompt":"Implemente a API. Nunca use rm -rf no projeto."}"#;
        assert!(
            matches!(
                evaluate(&s, "p", "mcp__orchestrator__cli_send", input),
                Verdict::Allow
            ),
            "mencionar um comando não é executá-lo — o gate da própria CLI cobre a execução"
        );
    }

    #[test]
    fn structural_fields_of_orchestrator_tools_are_still_checked() {
        let s = seeded();
        // O que a sandbox vai EXECUTAR continua passando pelo gate.
        let input = r#"{"name":"t","command":["sh","-c","rm -rf /work"]}"#;
        assert!(matches!(
            evaluate(&s, "p", "mcp__orchestrator__ui_exec", input),
            Verdict::Deny { .. }
        ));
    }

    #[test]
    fn normal_tools_are_evaluated_whole() {
        let s = seeded();
        // Bash não é tool do orquestrador: tudo conta, inclusive descrições.
        let input = r#"{"command":"rm -rf /tmp/x","description":"limpar"}"#;
        assert!(matches!(
            evaluate(&s, "p", "Bash", input),
            Verdict::Deny { .. }
        ));
    }

    #[test]
    fn actionable_input_drops_only_the_content_fields() {
        let out = actionable_input(
            "mcp__orchestrator__cli_send",
            r#"{"name":"x","prompt":"rm -rf","extra":1}"#,
        );
        assert!(out.contains("\"name\""));
        assert!(out.contains("\"extra\""));
        assert!(!out.contains("prompt"));
        // Entrada que não é JSON passa inteira (nada a filtrar).
        assert_eq!(actionable_input("mcp__orchestrator__cli_send", "solto"), "solto");
        // Tool de fora do orquestrador nunca é filtrada.
        let bash = r#"{"command":"x","description":"y"}"#;
        assert_eq!(actionable_input("Bash", bash), bash);
    }
}
