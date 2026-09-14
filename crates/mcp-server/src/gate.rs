//! A trava do Orchestrator: a mesma decisão para toda ferramenta de IA.
//!
//! O Claude Code, o Codex e o Kimi chamam o `orchestrator-hook` antes de cada
//! ferramenta; o Antigravity chama com outro formato; o OpenCode e o chat
//! HTTP não têm hook compatível e passam pelo servidor MCP em modo trava. Os
//! caminhos mudam, a decisão é esta:
//!
//! 1. regra `deny-regex` casou → nega e audita;
//! 2. a sessão ainda não consultou a memória e a ferramenta pode alterar
//!    algo → nega, pedindo `retrieve_memory` antes;
//! 3. regra `ask-regex` casou → vira decisão do dono. No modo autônomo, as
//!    ferramentas de orquestrar CLIs e a sandbox passam (auditadas): esse é o
//!    trabalho do orquestrador;
//! 4. nada casou → no modo autônomo libera explicitamente; senão fica em
//!    silêncio, para o pedido de permissão da própria ferramenta decidir.

use std::path::PathBuf;

use anyhow::Result;
use orchestrator_memory::contract;
use orchestrator_memory::daemon::{self, Request};
use orchestrator_memory::store::{
    MemoryStore, PendingDecision, REVIEWER_ORCHESTRATOR, REVIEWER_OWNER,
};

use crate::enforce::{evaluate, Verdict};
use crate::open_store;

/// A tool MCP de consulta — chamá-la destrava a sessão.
pub const RETRIEVE_TOOL: &str = "mcp__orchestrator__retrieve_memory";

/// Prefixo das tools MCP que dirigem CLIs (`cli_start`, `cli_send`, ...).
const CLI_TOOL_PREFIX: &str = "mcp__orchestrator__cli_";

/// Prefixo das tools de sandbox (`ui_open`, `ui_click`, ...). Testar dentro
/// de um container descartável é tão parte do trabalho quanto abrir CLI.
const UI_TOOL_PREFIX: &str = "mcp__orchestrator__ui_";

/// Uma chamada de ferramenta, já com o nome no formato canônico
/// ([`canonical_tool_name`]).
#[derive(Debug, Clone, Copy)]
pub struct ToolCall<'a> {
    pub tool_name: &'a str,
    pub tool_input: &'a str,
    pub session_id: &'a str,
    pub project: &'a str,
}

#[derive(Debug, Clone, Default)]
pub struct Options {
    /// Postura autônoma (`ORCHESTRATOR_AUTONOMOUS=1`): a trava é a única
    /// autoridade e libera explicitamente o que não casou regra.
    pub autonomous: bool,
    /// Avisar o dono por notificação do desktop quando algo pausar.
    pub notify: bool,
    /// Quem está pedindo (`ORCHESTRATOR_AGENT`): o nome da CLI, ou
    /// [`ORCHESTRATOR_AGENT`] quando é o próprio orquestrador.
    pub requester: Option<String>,
}

/// Como o chat do orquestrador se identifica (`ORCHESTRATOR_AGENT`).
pub const ORCHESTRATOR_AGENT: &str = "orquestrador";

/// Começo da resolução gravada quando quem decidiu foi o orquestrador.
pub const RESOLVED_BY_ORCHESTRATOR: &str = "orquestrador:";

/// O que responder à ferramenta de IA.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    /// Libera, com o motivo (auditável).
    Allow(String),
    /// Bloqueia, com o motivo que o modelo vai ler.
    Deny(String),
    /// Não opina: a ferramenta segue o próprio fluxo de permissão.
    Silent,
}

const CONSULT_FIRST: &str = "Antes de alterar qualquer coisa, consulte a memória do projeto: chame \
retrieve_memory sobre o que vai fazer (regras do dono, decisões, convenções, o que outras IAs já \
registraram). A primeira consulta libera esta sessão.";

const ALLOW_CLEAN: &str = "Liberado pelo Orchestrator (sem regra de segurança aplicável).";

/// Decide uma chamada de ferramenta.
pub fn decide(store: &MemoryStore, call: &ToolCall, opts: &Options) -> Result<Decision> {
    let ToolCall {
        tool_name,
        tool_input,
        session_id,
        project,
    } = *call;

    if tool_name == RETRIEVE_TOOL {
        let _ = store.record_consult(session_id, project);
    }
    let action = format!("{tool_name} {tool_input}");
    let is_cli_tool =
        tool_name.starts_with(CLI_TOOL_PREFIX) || tool_name.starts_with(UI_TOOL_PREFIX);
    let verdict = evaluate(store, project, tool_name, tool_input);

    // Obrigação do dono: nada que altere o projeto antes de consultar a
    // memória. Uma regra que BLOQUEIA continua vindo primeiro. Erro ao ler o
    // registro não trava a sessão: as regras seguem valendo.
    if !matches!(verdict, Verdict::Deny { .. })
        && needs_consult(tool_name)
        && !store.has_consulted(session_id, project).unwrap_or(true)
    {
        let _ = store.log_decision(
            project,
            session_id,
            &action,
            "blocked",
            "sessão ainda não consultou a memória",
        );
        return Ok(Decision::Deny(CONSULT_FIRST.to_string()));
    }

    Ok(match verdict {
        Verdict::Allow if opts.autonomous => Decision::Allow(ALLOW_CLEAN.to_string()),
        Verdict::Allow => Decision::Silent,
        Verdict::Deny { rule_title } => {
            let reason = format!(
                "Bloqueado pela regra de segurança do Orchestrator: \"{rule_title}\". \
                 Consulte a memória do projeto (retrieve_memory) e proponha uma alternativa."
            );
            let _ = store.log_decision(project, session_id, &action, "blocked", &reason);
            Decision::Deny(reason)
        }
        Verdict::Ask {
            rule_title,
            owner_only: false,
        } if opts.autonomous && is_cli_tool => {
            let reason = format!(
                "Liberado no modo autônomo: orquestrar CLIs é a função do \
                 orquestrador (regra \"{rule_title}\" não pausa tools cli_*)."
            );
            let _ = store.log_decision(project, session_id, &action, "allowed", &reason);
            Decision::Allow(reason)
        }
        Verdict::Ask {
            rule_title,
            owner_only,
        } => {
            let summary = format!("[{rule_title}] {tool_name}: {tool_input}");
            // Regra do dono: no modo autônomo, o pedido de uma CLI do
            // workspace é do ORQUESTRADOR — ele decide se bate com o que o
            // dono pediu e só escala o que for crítico demais ou que ele não
            // saiba. `owner-regex` e os pedidos do próprio orquestrador ficam
            // com o dono.
            let requester = opts.requester.as_deref().map(str::trim).unwrap_or("");
            let do_orquestrador = !owner_only
                && opts.autonomous
                && !requester.is_empty()
                && requester != ORCHESTRATOR_AGENT;
            match store.latest_decision(project, session_id, &summary)? {
                Some(d) if d.status == "approved" => {
                    let reason = format!(
                        "Liberado após aprovação {} (regra \"{rule_title}\").",
                        by_whom(&d)
                    );
                    let _ = store.log_decision(project, session_id, &action, "allowed", &reason);
                    Decision::Allow(reason)
                }
                Some(d) if d.status == "denied" => {
                    let motivo = d
                        .resolution
                        .as_deref()
                        .map(|r| r.trim_start_matches(RESOLVED_BY_ORCHESTRATOR).trim())
                        .filter(|r| !r.is_empty())
                        .map(|r| format!(": {r}"))
                        .unwrap_or_default();
                    let reason = format!(
                        "Negado {} (regra \"{rule_title}\"){motivo}.",
                        by_whom(&d)
                    );
                    let _ = store.log_decision(project, session_id, &action, "blocked", &reason);
                    Decision::Deny(reason)
                }
                Some(d) if d.status == "pending" => Decision::Deny(waiting_text(&d, &rule_title)),
                _ => {
                    let revisor = if do_orquestrador {
                        REVIEWER_ORCHESTRATOR
                    } else {
                        REVIEWER_OWNER
                    };
                    let pending = store.enqueue_decision_for(
                        project,
                        session_id,
                        &summary,
                        revisor,
                        requester,
                    )?;
                    if opts.notify && pending.is_for_owner() {
                        orchestrator_notify::notify_pending_decision(project, &summary);
                    }
                    Decision::Deny(format!(
                        "{} Pendência {} registrada.",
                        waiting_text(&pending, &rule_title),
                        &pending.id[..8.min(pending.id.len())]
                    ))
                }
            }
        }
    })
}

/// "pelo orquestrador" ou "pelo dono", pela resolução gravada.
fn by_whom(d: &PendingDecision) -> &'static str {
    if d
        .resolution
        .as_deref()
        .is_some_and(|r| r.starts_with(RESOLVED_BY_ORCHESTRATOR))
    {
        "pelo orquestrador"
    } else {
        "pelo dono"
    }
}

/// O que a IA lê enquanto a decisão não sai: quem vai decidir e o que fazer.
fn waiting_text(d: &PendingDecision, rule_title: &str) -> String {
    if d.is_for_owner() {
        format!(
            "Ação pausada pelo Orchestrator (regra \"{rule_title}\"): aguardando a decisão do \
             dono. Siga outra frente do trabalho e tente de novo depois."
        )
    } else {
        format!(
            "Ação pausada pelo Orchestrator (regra \"{rule_title}\"): o orquestrador vai decidir \
             se isso bate com o que o dono pediu. Siga outra frente e tente de novo quando ele \
             avisar."
        )
    }
}

/// Ferramentas livres da consulta prévia: as que LEEM, pesquisam, perguntam
/// ou mexem só na memória do próprio Orchestrator. Tudo que pode alterar o
/// projeto ou comandar outra IA espera a primeira consulta da sessão; nome
/// desconhecido também espera (na dúvida, trava).
const FREE_WITHOUT_CONSULT: &[&str] = &[
    // Claude Code
    "Read",
    "Grep",
    "Glob",
    "LS",
    "NotebookRead",
    "WebSearch",
    "WebFetch",
    "ToolSearch",
    "TodoWrite",
    "AskUserQuestion",
    "EnterPlanMode",
    "ExitPlanMode",
    "ListMcpResourcesTool",
    "ReadMcpResourceTool",
    "BashOutput",
    // Kimi Code
    "ReadFile",
    "SearchWeb",
    "FetchURL",
    "Think",
    "SetTodoList",
    // Antigravity (tipos de passo em minúsculas)
    "view_file",
    "list_dir",
    "grep_search",
    "find_by_name",
    "search_web",
    "read_url_content",
    "read_resource",
    // Orchestrator
    RETRIEVE_TOOL,
    "mcp__orchestrator__list_memories",
    "mcp__orchestrator__store_memory",
    "mcp__orchestrator__log_decision",
    "mcp__orchestrator__permission_prompt",
    "mcp__orchestrator__cli_status",
    "mcp__orchestrator__cli_read",
    "mcp__orchestrator__cli_menu",
    "mcp__orchestrator__ui_snapshot",
    "mcp__orchestrator__ui_screenshot",
];

/// Esta ferramenta exige consulta prévia à memória?
pub fn needs_consult(tool_name: &str) -> bool {
    !FREE_WITHOUT_CONSULT.contains(&tool_name)
}

/// Tools do servidor MCP do Orchestrator, sem prefixo.
fn is_our_tool(name: &str) -> bool {
    matches!(
        name,
        "retrieve_memory" | "store_memory" | "list_memories" | "log_decision" | "permission_prompt"
    ) || name.starts_with("cli_")
        || name.starts_with("ui_")
        || name.starts_with("decision_")
        || name == "ask_owner"
}

/// O nome de uma ferramenta no formato das regras e das listas daqui (o do
/// Claude Code).
///
/// Cada ferramenta de IA nomeia as tools MCP do seu jeito
/// (`mcp__orchestrator__cli_start`, `orchestrator__cli_start`,
/// `orchestrator.cli_start`), e dentro do próprio servidor ela chega sem
/// prefixo nenhum. Todas viram `mcp__orchestrator__cli_start`; o resto passa
/// intacto.
pub fn canonical_tool_name(raw: &str) -> String {
    for prefix in [
        "mcp__orchestrator__",
        "orchestrator__",
        "orchestrator.",
        "orchestrator/",
        "orchestrator_",
    ] {
        if let Some(rest) = raw.strip_prefix(prefix) {
            if is_our_tool(rest) {
                return format!("mcp__orchestrator__{rest}");
            }
        }
    }
    if is_our_tool(raw) {
        return format!("mcp__orchestrator__{raw}");
    }
    raw.to_string()
}

/// O texto invisível de um prompt: contrato, regras fixas do dono e o
/// índice escolhido para ele.
///
/// Pede ao memoryd (busca semântica). Se ele não responde, dispara o memoryd
/// para os próximos prompts e monta o texto pelo caminho léxico. Nunca
/// atrapalha o prompt: falha aqui só deixa de injetar o contexto (`None`).
pub fn prompt_context(prompt: &str, project: &str, author: &str, db: Option<PathBuf>) -> Option<String> {
    let pedido = Request::Context {
        project: project.to_string(),
        prompt: prompt.to_string(),
        author: author.to_string(),
    };
    let texto = match daemon::call(&pedido, daemon::REQUEST_TIMEOUT) {
        Ok(resposta) => resposta.text,
        Err(_) => {
            let _ = daemon::spawn_detached();
            match open_store(db).and_then(|s| contract::lexical_context(&s, project, prompt, author)) {
                Ok(t) => t,
                Err(e) => {
                    eprintln!("orchestrator-hook: sem contexto de memória: {e:#}");
                    return None;
                }
            }
        }
    };
    (!texto.trim().is_empty()).then_some(texto)
}

#[cfg(test)]
mod tests {
    use super::*;
    use orchestrator_memory::MemoryKind;

    fn store() -> MemoryStore {
        let s = MemoryStore::open_in_memory().unwrap();
        s.add_memory("p", MemoryKind::Security, "nunca rm -rf", "deny-regex: rm\\s+-rf", 10)
            .unwrap();
        s.add_memory("p", MemoryKind::Security, "migrações pedem aprovação", "ask-regex: (?i)migrate", 5)
            .unwrap();
        s
    }

    fn call<'a>(tool: &'a str, input: &'a str) -> ToolCall<'a> {
        ToolCall {
            tool_name: tool,
            tool_input: input,
            session_id: "s",
            project: "p",
        }
    }

    // Sem notificação de desktop nos testes.
    const AUTO: Options = Options { autonomous: true, notify: false, requester: None };
    const PERGUNTA: Options = Options { autonomous: false, notify: false, requester: None };

    fn cli(nome: &str) -> Options {
        Options { autonomous: true, notify: false, requester: Some(nome.into()) }
    }

    #[test]
    fn in_autonomous_mode_a_cli_request_goes_to_the_orchestrator() {
        let s = store();
        let backend = cli("backend");
        decide(&s, &call(RETRIEVE_TOOL, "{}"), &backend).unwrap();
        let migrar = call("Bash", r#"{"command":"sqlx migrate run"}"#);
        let d = decide(&s, &migrar, &backend).unwrap();
        assert!(matches!(&d, Decision::Deny(m) if m.contains("o orquestrador vai decidir")), "{d:?}");
        let pendentes = s.list_pending_decisions(true).unwrap();
        assert!(!pendentes[0].is_for_owner());
        assert_eq!(pendentes[0].requester, "backend");
        // O orquestrador negou: a CLI lê quem negou e por quê.
        s.resolve_decision(&pendentes[0].id, false, "orquestrador: o dono não pediu migração")
            .unwrap();
        let d = decide(&s, &migrar, &backend).unwrap();
        assert!(
            matches!(&d, Decision::Deny(m) if m.contains("pelo orquestrador") && m.contains("o dono não pediu migração")),
            "{d:?}"
        );
    }

    #[test]
    fn owner_rules_and_the_orchestrators_own_requests_stay_with_the_owner() {
        let s = store();
        s.add_memory("p", MemoryKind::Security, "produção é comigo", "owner-regex: --env\\s+prod", 1)
            .unwrap();
        let backend = cli("backend");
        decide(&s, &call(RETRIEVE_TOOL, "{}"), &backend).unwrap();
        decide(&s, &call("Bash", r#"{"command":"deploy --env prod"}"#), &backend).unwrap();
        let proprio = cli(ORCHESTRATOR_AGENT);
        decide(&s, &call("Bash", r#"{"command":"sqlx migrate run"}"#), &proprio).unwrap();
        let pendentes = s.list_pending_decisions(true).unwrap();
        assert_eq!(pendentes.len(), 2);
        assert!(pendentes.iter().all(|p| p.is_for_owner()), "{pendentes:?}");
    }

    #[test]
    fn changing_things_waits_for_the_first_consult() {
        let s = store();
        let ls = call("Bash", r#"{"command":"ls"}"#);
        let d = decide(&s, &ls, &AUTO).unwrap();
        assert!(matches!(&d, Decision::Deny(m) if m.contains("retrieve_memory")), "{d:?}");
        // Ler é livre.
        assert!(matches!(decide(&s, &call("Read", "{}"), &AUTO).unwrap(), Decision::Allow(_)));
        decide(&s, &call(RETRIEVE_TOOL, r#"{"query":"x"}"#), &AUTO).unwrap();
        assert!(matches!(decide(&s, &ls, &AUTO).unwrap(), Decision::Allow(_)));
        // Sem postura autônoma, ferramenta limpa fica por conta da própria CLI.
        assert_eq!(decide(&s, &ls, &PERGUNTA).unwrap(), Decision::Silent);
    }

    #[test]
    fn unknown_tool_names_also_wait_for_the_consult() {
        let s = store();
        let d = decide(&s, &call("run_command", "{}"), &AUTO).unwrap();
        assert!(matches!(d, Decision::Deny(_)));
    }

    #[test]
    fn deny_rule_wins_even_before_the_consult() {
        let s = store();
        let d = decide(&s, &call("Bash", r#"{"command":"rm -rf /"}"#), &AUTO).unwrap();
        assert!(matches!(&d, Decision::Deny(m) if m.contains("nunca rm -rf")), "{d:?}");
    }

    #[test]
    fn ask_rule_pauses_until_the_owner_decides() {
        let s = store();
        decide(&s, &call(RETRIEVE_TOOL, "{}"), &AUTO).unwrap();
        let migrar = call("Bash", r#"{"command":"sqlx migrate run"}"#);
        let d = decide(&s, &migrar, &AUTO).unwrap();
        assert!(matches!(&d, Decision::Deny(m) if m.contains("Pendência")), "{d:?}");
        let pendentes = s.list_pending_decisions(true).unwrap();
        assert_eq!(pendentes.len(), 1);
        // Tentar de novo não duplica a pendência.
        decide(&s, &migrar, &AUTO).unwrap();
        assert_eq!(s.list_pending_decisions(true).unwrap().len(), 1);
        s.resolve_decision(&pendentes[0].id, true, "aprovado no teste").unwrap();
        assert!(matches!(decide(&s, &migrar, &AUTO).unwrap(), Decision::Allow(_)));
    }

    #[test]
    fn orchestrating_clis_is_not_paused_in_autonomous_mode() {
        let s = store();
        decide(&s, &call(RETRIEVE_TOOL, "{}"), &AUTO).unwrap();
        let cli = call("mcp__orchestrator__cli_start", r#"{"name":"migrate"}"#);
        assert!(matches!(decide(&s, &cli, &AUTO).unwrap(), Decision::Allow(_)));
    }

    #[test]
    fn our_tools_get_the_same_name_in_every_harness() {
        for bruto in [
            "mcp__orchestrator__cli_start",
            "orchestrator__cli_start",
            "orchestrator.cli_start",
            "cli_start",
        ] {
            assert_eq!(canonical_tool_name(bruto), "mcp__orchestrator__cli_start", "{bruto}");
        }
        assert_eq!(canonical_tool_name("Bash"), "Bash");
        assert_eq!(canonical_tool_name("orchestrator_notas"), "orchestrator_notas");
    }
}
