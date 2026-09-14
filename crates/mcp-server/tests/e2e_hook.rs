//! E2E do enforcement: exercita o binário `orchestrator-hook` REAL pelo
//! mesmo contrato JSON que o Claude Code usa no evento `PreToolUse`.
//!
//! Não simula teclado nem mocka o hook: seedamos uma regra de `security`
//! com `deny-regex` num SQLite temporário, invocamos o executável compilado
//! (`CARGO_BIN_EXE_orchestrator-hook`) com o payload da tool call no stdin e
//! os envs `ORCHESTRATOR_DB`/`ORCHESTRATOR_PROJECT`, e verificamos:
//!   1. o stdout responde `permissionDecision: "deny"` (o Claude Code bloqueia);
//!   2. o `decisions_log` registrou a auditoria com `decision = "blocked"`.
//!
//! Uma tool call limpa (sem casar regra) deve sair vazia e NÃO bloquear.
//!
//! Duas obrigações passam pelo mesmo binário e têm testes aqui também: a
//! TRAVA de consulta (nada que altere o projeto antes de `retrieve_memory`
//! na sessão) e o CONTEXTO invisível do `UserPromptSubmit`.
//!
//! O caminho de sessão `claude` ao vivo fica em `scripts/e2e-live.sh`
//! (precisa do binário `claude` autenticado + rede) e não roda no CI.

use std::io::Write;
use std::process::{Command, Stdio};

use orchestrator_memory::store::MemoryStore;
use orchestrator_memory::MemoryKind;
use serde_json::{json, Value};

const HOOK_BIN: &str = env!("CARGO_BIN_EXE_orchestrator-hook");

/// Roda o hook compilado com `payload` no stdin e o db/projeto dados nos envs.
/// Devolve (stdout, stderr, sucesso).
fn run_hook(db_path: &str, project: &str, payload: &Value) -> (String, String, bool) {
    run_hook_env(db_path, project, payload, &[])
}

/// Como [`run_hook`], mas com envs extras (ex.: `ORCHESTRATOR_AUTONOMOUS`).
fn run_hook_env(
    db_path: &str,
    project: &str,
    payload: &Value,
    envs: &[(&str, &str)],
) -> (String, String, bool) {
    let mut cmd = Command::new(HOOK_BIN);
    cmd.env("ORCHESTRATOR_DB", db_path)
        .env("ORCHESTRATOR_PROJECT", project);
    // Hermético: nunca sobe um memoryd de verdade (o binário testado fica ao
    // lado dele e carregaria modelos) nem conversa com um que já rode aqui.
    cmd.env("ORCHESTRATOR_MEMORYD_AUTOSTART", "0").env(
        "ORCHESTRATOR_MEMORYD_SOCKET",
        std::env::temp_dir().join("orchestrator-e2e-sem-memoryd.sock"),
    );
    for (k, v) in envs {
        cmd.env(k, v);
    }
    let mut child = cmd
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("falha ao spawnar o binário orchestrator-hook");

    child
        .stdin
        .take()
        .expect("stdin do hook")
        .write_all(payload.to_string().as_bytes())
        .expect("escrevendo payload no stdin do hook");

    let out = child.wait_with_output().expect("aguardando o hook");
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        out.status.success(),
    )
}

/// Seeda um SQLite temporário com uma regra security `deny-regex: rm -rf`.
/// Devolve (tempdir guard, caminho do db como String).
fn seed_db() -> (tempfile::TempDir, String) {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("memory.db");
    {
        let store =
            MemoryStore::open(&db_path).expect("abrir store");
        store
            .add_memory(
                "e2e",
                MemoryKind::Security,
                "nunca rodar rm -rf",
                "Comandos destrutivos são proibidos.\ndeny-regex: rm\\s+-rf",
                10,
            )
            .expect("seed da regra de segurança");
        consultar_sessoes(&store);
    } // fecha o store (libera o arquivo) antes de o hook reabrir
    (dir, db_path.display().to_string())
}

/// As sessões dos testes de SEGURANÇA já consultaram a memória: o que eles
/// verificam são as regras, não a trava de consulta (que tem testes próprios
/// com sessões novas, mais abaixo).
fn consultar_sessoes(store: &MemoryStore) {
    for sessao in ["e2e-session-0001", "e2e-session-cli", "e2e-session-ui"] {
        store.record_consult(sessao, "e2e").expect("registrar consulta");
    }
}

/// Payload idêntico ao que o Claude Code envia no `PreToolUse`.
fn pretooluse(command: &str) -> Value {
    json!({
        "session_id": "e2e-session-0001",
        "cwd": "/tmp/projeto-e2e",
        "hook_event_name": "PreToolUse",
        "tool_name": "Bash",
        "tool_input": { "command": command }
    })
}

#[test]
fn hook_denies_destructive_command_and_audits_blocked() {
    let (_dir, db_path) = seed_db();

    let (stdout, stderr, ok) = run_hook(&db_path, "e2e", &pretooluse("rm -rf /tmp/x"));
    assert!(ok, "hook deve sair com sucesso; stderr: {stderr}");

    // 1. contrato de saída do Claude Code: deny.
    let out: Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("stdout do hook não é JSON ({e}): {stdout:?}"));
    assert_eq!(
        out["hookSpecificOutput"]["permissionDecision"]
            .as_str()
            .unwrap_or(""),
        "deny",
        "a tool call destrutiva deveria ser bloqueada; saída: {out}"
    );

    // 2. auditoria append-only registrou o bloqueio.
    let store =
        MemoryStore::open(&db_path).expect("reabrir store");
    let decisions = store.list_decisions("e2e").expect("listar decisões");
    let blocked = decisions
        .iter()
        .find(|d| d.decision == "blocked")
        .expect("deveria existir uma decisão 'blocked' no log");
    assert!(
        blocked.action.contains("rm -rf") && blocked.action.contains("Bash"),
        "a ação auditada deveria conter a tool call; foi: {:?}",
        blocked.action
    );
    assert_eq!(blocked.session_id, "e2e-session-0001");
}

#[test]
fn hook_allows_clean_command_without_blocking() {
    let (_dir, db_path) = seed_db();

    let (stdout, stderr, ok) = run_hook(&db_path, "e2e", &pretooluse("ls -la"));
    assert!(ok, "hook deve sair com sucesso; stderr: {stderr}");

    // Nada casou: saída vazia, sem bloqueio.
    assert!(
        stdout.trim().is_empty(),
        "comando limpo não deveria produzir decisão; saída: {stdout:?}"
    );

    let store =
        MemoryStore::open(&db_path).expect("reabrir store");
    let decisions = store.list_decisions("e2e").expect("listar decisões");
    assert!(
        decisions.is_empty(),
        "comando limpo não deveria gerar auditoria; havia: {decisions:?}"
    );
}

#[test]
fn hook_allows_clean_command_explicitly_when_autonomous() {
    let (_dir, db_path) = seed_db();

    // Postura autônoma: o hook é a autoridade e libera tools limpas.
    let (stdout, stderr, ok) = run_hook_env(
        &db_path,
        "e2e",
        &pretooluse("ls -la"),
        &[("ORCHESTRATOR_AUTONOMOUS", "1")],
    );
    assert!(ok, "hook deve sair com sucesso; stderr: {stderr}");

    let out: Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("stdout do hook não é JSON ({e}): {stdout:?}"));
    assert_eq!(
        out["hookSpecificOutput"]["permissionDecision"]
            .as_str()
            .unwrap_or(""),
        "allow",
        "em modo autônomo, a tool limpa deveria ser liberada explicitamente; saída: {out}"
    );

    // Liberação de tool limpa não gera auditoria (só decisões de segurança).
    let store =
        MemoryStore::open(&db_path).expect("reabrir store");
    assert!(store.list_decisions("e2e").expect("listar decisões").is_empty());
}

/// Seeda um SQLite temporário com uma regra `ask-regex` que casa QUALQUER
/// tool call de CLI (pega o nome da tool no corpo avaliado) e também um
/// `deny-regex` específico. Devolve (tempdir guard, caminho do db).
fn seed_db_with_cli_rules() -> (tempfile::TempDir, String) {
    let dir = tempfile::tempdir().expect("tempdir");
    let db_path = dir.path().join("memory.db");
    {
        let store =
            MemoryStore::open(&db_path).expect("abrir store");
        store
            .add_memory(
                "e2e",
                MemoryKind::Security,
                "confirmar abertura de CLI",
                "Abrir CLI pede confirmação.\nask-regex: cli_start",
                5,
            )
            .expect("seed da regra ask");
        store
            .add_memory(
                "e2e",
                MemoryKind::Security,
                "CLI proibida",
                "A CLI de produção é proibida.\ndeny-regex: producao",
                10,
            )
            .expect("seed da regra deny");
        consultar_sessoes(&store);
    }
    (dir, db_path.display().to_string())
}

/// Payload de uma tool call MCP de CLI (o que o `claude` do orquestrador
/// envia quando chama `cli_start`).
fn pretooluse_cli(tool: &str, name: &str) -> Value {
    json!({
        "session_id": "e2e-session-cli",
        "cwd": "/tmp/projeto-e2e",
        "hook_event_name": "PreToolUse",
        "tool_name": format!("mcp__orchestrator__{tool}"),
        "tool_input": { "name": name }
    })
}

#[test]
fn hook_allows_cli_tools_in_autonomous_mode_despite_ask_rule() {
    let (_dir, db_path) = seed_db_with_cli_rules();

    let (stdout, stderr, ok) = run_hook_env(
        &db_path,
        "e2e",
        &pretooluse_cli("cli_start", "frontend"),
        &[("ORCHESTRATOR_AUTONOMOUS", "1")],
    );
    assert!(ok, "hook deve sair com sucesso; stderr: {stderr}");

    let out: Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("stdout do hook não é JSON ({e}): {stdout:?}"));
    assert_eq!(
        out["hookSpecificOutput"]["permissionDecision"]
            .as_str()
            .unwrap_or(""),
        "allow",
        "no modo autônomo, abrir CLI não deveria pedir permissão; saída: {out}"
    );

    // A liberação por desenho fica auditada (visível no F2).
    let store =
        MemoryStore::open(&db_path).expect("reabrir store");
    let decisions = store.list_decisions("e2e").expect("listar decisões");
    let allowed = decisions
        .iter()
        .find(|d| d.decision == "allowed")
        .expect("deveria auditar a liberação automática");
    assert!(allowed.action.contains("cli_start"));
    // E nada foi para a fila de decisões pendentes.
    assert!(store
        .list_pending_decisions(true)
        .expect("listar pendências")
        .is_empty());
}

#[test]
fn hook_still_asks_for_cli_tools_when_not_autonomous() {
    let (_dir, db_path) = seed_db_with_cli_rules();

    // Sem ORCHESTRATOR_AUTONOMOUS: a regra `ask-regex` volta a valer.
    let (stdout, stderr, ok) = run_hook(&db_path, "e2e", &pretooluse_cli("cli_start", "frontend"));
    assert!(ok, "hook deve sair com sucesso; stderr: {stderr}");

    let out: Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("stdout do hook não é JSON ({e}): {stdout:?}"));
    assert_eq!(
        out["hookSpecificOutput"]["permissionDecision"]
            .as_str()
            .unwrap_or(""),
        "deny",
        "fora do modo autônomo, a regra ask deveria pausar; saída: {out}"
    );

    let store =
        MemoryStore::open(&db_path).expect("reabrir store");
    assert_eq!(
        store
            .list_pending_decisions(true)
            .expect("listar pendências")
            .len(),
        1,
        "a decisão deveria ter entrado na fila para o usuário"
    );
}

#[test]
fn hook_denies_cli_tool_matching_deny_rule_even_in_autonomous_mode() {
    let (_dir, db_path) = seed_db_with_cli_rules();

    // deny-regex tem precedência ABSOLUTA: a exceção do autônomo não a fura.
    let (stdout, stderr, ok) = run_hook_env(
        &db_path,
        "e2e",
        &pretooluse_cli("cli_start", "producao"),
        &[("ORCHESTRATOR_AUTONOMOUS", "1")],
    );
    assert!(ok, "hook deve sair com sucesso; stderr: {stderr}");

    let out: Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("stdout do hook não é JSON ({e}): {stdout:?}"));
    assert_eq!(
        out["hookSpecificOutput"]["permissionDecision"]
            .as_str()
            .unwrap_or(""),
        "deny",
        "regra deny deveria bloquear mesmo no autônomo; saída: {out}"
    );

    let store =
        MemoryStore::open(&db_path).expect("reabrir store");
    assert!(store
        .list_decisions("e2e")
        .expect("listar decisões")
        .iter()
        .any(|d| d.decision == "blocked"));
}

#[test]
fn hook_allows_sandbox_tools_in_autonomous_mode() {
    let (_dir, db_path) = seed_db_with_cli_rules();

    // Testar numa sandbox isolada não deve pedir permissão no autônomo.
    let (stdout, stderr, ok) = run_hook_env(
        &db_path,
        "e2e",
        &json!({
            "session_id": "e2e-session-ui",
            "cwd": "/tmp/projeto-e2e",
            "hook_event_name": "PreToolUse",
            "tool_name": "mcp__orchestrator__ui_open",
            "tool_input": { "name": "teste", "url": "localhost:3000" }
        }),
        &[("ORCHESTRATOR_AUTONOMOUS", "1")],
    );
    assert!(ok, "hook deve sair com sucesso; stderr: {stderr}");
    let out: Value = serde_json::from_str(stdout.trim()).unwrap();
    assert_eq!(
        out["hookSpecificOutput"]["permissionDecision"]
            .as_str()
            .unwrap_or(""),
        "allow"
    );
}

#[test]
fn hook_still_blocks_a_sandbox_tool_that_matches_deny() {
    let (_dir, db_path) = seed_db_with_cli_rules();
    // A regra deny casa "producao" — vale também para a sandbox.
    let (stdout, _, ok) = run_hook_env(
        &db_path,
        "e2e",
        &json!({
            "session_id": "e2e-session-ui",
            "cwd": "/tmp/projeto-e2e",
            "hook_event_name": "PreToolUse",
            "tool_name": "mcp__orchestrator__ui_exec",
            "tool_input": { "name": "t", "command": ["deploy-producao"] }
        }),
        &[("ORCHESTRATOR_AUTONOMOUS", "1")],
    );
    assert!(ok);
    let out: Value = serde_json::from_str(stdout.trim()).unwrap();
    assert_eq!(
        out["hookSpecificOutput"]["permissionDecision"]
            .as_str()
            .unwrap_or(""),
        "deny"
    );
}

/// Uma tool call numa sessão qualquer.
fn tool_call(session: &str, tool: &str, input: Value) -> Value {
    json!({
        "session_id": session,
        "cwd": "/tmp/projeto-e2e",
        "hook_event_name": "PreToolUse",
        "tool_name": tool,
        "tool_input": input
    })
}

/// A decisão que o hook devolveu (vazia quando ficou em silêncio).
fn decisao(stdout: &str) -> String {
    serde_json::from_str::<Value>(stdout.trim())
        .ok()
        .and_then(|v| {
            v["hookSpecificOutput"]["permissionDecision"]
                .as_str()
                .map(str::to_string)
        })
        .unwrap_or_default()
}

#[test]
fn nothing_changes_the_project_before_the_session_consults_memory() {
    let (_dir, db_path) = seed_db();
    let sessao = "e2e-sessao-nova";
    let auto = [("ORCHESTRATOR_AUTONOMOUS", "1")];

    // 1. Sem consulta: até um comando limpo é negado, ensinando o caminho.
    let (stdout, stderr, ok) = run_hook_env(
        &db_path,
        "e2e",
        &tool_call(sessao, "Bash", json!({ "command": "ls -la" })),
        &auto,
    );
    assert!(ok, "stderr: {stderr}");
    assert_eq!(decisao(&stdout), "deny", "{stdout}");
    assert!(
        stdout.contains("retrieve_memory"),
        "a razão tem de ensinar o próximo passo: {stdout}"
    );
    let store = MemoryStore::open(&db_path).expect("reabrir");
    assert!(
        store
            .list_decisions("e2e")
            .unwrap()
            .iter()
            .any(|d| d.decision == "blocked" && d.reason.contains("não consultou")),
        "o bloqueio pela trava fica auditado"
    );
    drop(store);

    // 2. Ler é livre — é com leitura que se consulta.
    let (stdout, _, _) = run_hook_env(
        &db_path,
        "e2e",
        &tool_call(sessao, "Read", json!({ "file_path": "/tmp/x" })),
        &auto,
    );
    assert_eq!(decisao(&stdout), "allow", "{stdout}");

    // 3. A consulta passa e fica registrada.
    let (stdout, _, _) = run_hook_env(
        &db_path,
        "e2e",
        &tool_call(
            sessao,
            "mcp__orchestrator__retrieve_memory",
            json!({ "query": "listar arquivos" }),
        ),
        &auto,
    );
    assert_eq!(decisao(&stdout), "allow", "{stdout}");
    let store = MemoryStore::open(&db_path).expect("reabrir");
    assert!(store.has_consulted(sessao, "e2e").unwrap());
    drop(store);

    // 4. Agora o mesmo comando passa.
    let (stdout, _, _) = run_hook_env(
        &db_path,
        "e2e",
        &tool_call(sessao, "Bash", json!({ "command": "ls -la" })),
        &auto,
    );
    assert_eq!(decisao(&stdout), "allow", "{stdout}");

    // 5. E a regra que BLOQUEIA continua valendo depois da consulta.
    let (stdout, _, _) = run_hook_env(
        &db_path,
        "e2e",
        &tool_call(sessao, "Bash", json!({ "command": "rm -rf /tmp/x" })),
        &auto,
    );
    assert_eq!(decisao(&stdout), "deny", "{stdout}");
}

#[test]
fn consulting_in_one_session_does_not_free_another() {
    let (_dir, db_path) = seed_db();
    let auto = [("ORCHESTRATOR_AUTONOMOUS", "1")];
    let escrever = json!({ "file_path": "/tmp/x", "content": "y" });
    run_hook_env(
        &db_path,
        "e2e",
        &tool_call("sessao-a", "mcp__orchestrator__retrieve_memory", json!({ "query": "x" })),
        &auto,
    );
    let (stdout, _, _) = run_hook_env(
        &db_path,
        "e2e",
        &tool_call("sessao-b", "Write", escrever.clone()),
        &auto,
    );
    assert_eq!(decisao(&stdout), "deny", "a sessão B não consultou: {stdout}");
    let (stdout, _, _) =
        run_hook_env(&db_path, "e2e", &tool_call("sessao-a", "Write", escrever), &auto);
    assert_eq!(decisao(&stdout), "allow", "a sessão A consultou: {stdout}");
}

#[test]
fn every_prompt_gets_the_memory_contract_even_without_memoryd() {
    let (_dir, db_path) = seed_db();
    {
        let store = MemoryStore::open(&db_path).expect("abrir");
        store
            .add(orchestrator_memory::store::NewMemory::global(
                MemoryKind::Practice,
                "commits em português",
                "mensagens curtas, no imperativo",
                9,
            ))
            .expect("global");
        store
            .add_memory(
                "e2e",
                MemoryKind::Architecture,
                "deploy pelo fly.io",
                "flyctl deploy a partir da main",
                3,
            )
            .expect("projeto");
    }
    let (stdout, stderr, ok) = run_hook_env(
        &db_path,
        "e2e",
        &json!({
            "session_id": "e2e-prompt",
            "cwd": "/tmp/projeto-e2e",
            "hook_event_name": "UserPromptSubmit",
            "prompt": "como faço o deploy?"
        }),
        &[("ORCHESTRATOR_AGENT", "frontend")],
    );
    assert!(ok, "o hook de prompt nunca pode falhar: {stderr}");
    let out: Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("stdout não é JSON ({e}): {stdout:?}"));
    assert_eq!(out["hookSpecificOutput"]["hookEventName"], "UserPromptSubmit");
    let ctx = out["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .unwrap_or("");
    assert!(ctx.contains("<orchestrator-memory>"), "{ctx}");
    assert!(ctx.contains("palavras-chave"), "sem memoryd o índice é léxico e diz isso: {ctx}");
    assert!(ctx.contains("commits em português"), "regra fixa global: {ctx}");
    assert!(ctx.contains("deploy pelo fly.io"), "índice do prompt: {ctx}");
    assert!(ctx.contains("frontend"), "a IA é chamada pelo nome: {ctx}");
    // Receber o contexto não conta como consulta: a trava continua valendo.
    let store = MemoryStore::open(&db_path).unwrap();
    assert!(!store.has_consulted("e2e-prompt", "e2e").unwrap());
}
