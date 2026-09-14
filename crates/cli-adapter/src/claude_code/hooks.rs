//! Geração da configuração de hooks (`.claude/settings.json`) e de
//! servidores MCP (`.mcp.json`) do projeto.
//!
//! Funções puras que produzem `serde_json::Value` + helpers de filesystem
//! com semântica *write-if-changed* e merge cuidadoso: só as chaves que
//! este orquestrador possui são alteradas; o restante do arquivo do
//! usuário é preservado.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde_json::{json, Map, Value};

/// Parâmetros para a configuração de hooks gerada pelo orquestrador.
#[derive(Debug, Clone)]
pub struct HooksConfig {
    /// Arquivo de regras injetado no início da sessão (via `cat`).
    pub rules_file: String,
    /// Caminho do binário de hook do orquestrador, chamado em `PreToolUse`
    /// (gate de cada ferramenta) e em `UserPromptSubmit` (contexto de memória
    /// invisível de cada prompt).
    pub orchestrator_hook_bin: String,
}

/// Constrói o valor da chave `hooks` que o orquestrador possui:
/// `SessionStart` injeta regras, `UserPromptSubmit` injeta o contexto de
/// memória de cada prompt e `PreToolUse` passa cada ferramenta pelo gate.
pub fn build_hooks_value(cfg: &HooksConfig) -> Value {
    json!({
        "SessionStart": [
            {
                "hooks": [
                    {
                        "type": "command",
                        "command": format!("cat \"{}\"", cfg.rules_file)
                    }
                ]
            }
        ],
        "UserPromptSubmit": [
            {
                "hooks": [
                    {
                        "type": "command",
                        "command": cfg.orchestrator_hook_bin.clone()
                    }
                ]
            }
        ],
        "PreToolUse": [
            {
                "matcher": "*",
                "hooks": [
                    {
                        "type": "command",
                        "command": cfg.orchestrator_hook_bin.clone()
                    }
                ]
            }
        ]
    })
}

/// Faz o merge da configuração de hooks em um `settings.json` existente.
///
/// Apenas as entradas `hooks.SessionStart`, `hooks.UserPromptSubmit` e
/// `hooks.PreToolUse` são substituídas; todas as outras chaves (do topo e dentro de `hooks`)
/// são preservadas. Se `existing` não for um objeto, é substituído.
pub fn merge_settings(existing: Option<Value>, cfg: &HooksConfig) -> Value {
    let mut root = match existing {
        Some(Value::Object(m)) => m,
        _ => Map::new(),
    };
    let mut hooks = match root.remove("hooks") {
        Some(Value::Object(m)) => m,
        _ => Map::new(),
    };
    let ours = build_hooks_value(cfg);
    if let Value::Object(ours) = ours {
        for (k, v) in ours {
            hooks.insert(k, v);
        }
    }
    root.insert("hooks".to_owned(), Value::Object(hooks));
    Value::Object(root)
}

/// Constrói o conteúdo de `.mcp.json` apontando para um servidor MCP.
pub fn build_mcp_value(server_name: &str, command: &str, args: &[String]) -> Value {
    json!({
        "mcpServers": {
            server_name: {
                "command": command,
                "args": args,
            }
        }
    })
}

/// Faz o merge de um servidor MCP em um `.mcp.json` existente, tocando
/// apenas `mcpServers.<server_name>`.
pub fn merge_mcp(existing: Option<Value>, server_name: &str, command: &str, args: &[String]) -> Value {
    let mut root = match existing {
        Some(Value::Object(m)) => m,
        _ => Map::new(),
    };
    let mut servers = match root.remove("mcpServers") {
        Some(Value::Object(m)) => m,
        _ => Map::new(),
    };
    servers.insert(
        server_name.to_owned(),
        json!({ "command": command, "args": args }),
    );
    root.insert("mcpServers".to_owned(), Value::Object(servers));
    Value::Object(root)
}

/// Escreve `value` como JSON identado em `path` apenas se o conteúdo
/// mudou. Cria diretórios pais se necessário. Retorna `true` se escreveu.
pub fn write_json_if_changed(path: &Path, value: &Value) -> Result<bool> {
    let new_content = format!("{}\n", serde_json::to_string_pretty(value)?);
    if let Ok(current) = fs::read_to_string(path) {
        // Compara semanticamente para não reescrever por formatação.
        if let Ok(cur_val) = serde_json::from_str::<Value>(&current) {
            if &cur_val == value {
                return Ok(false);
            }
        }
        if current == new_content {
            return Ok(false);
        }
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("criando diretório {}", parent.display()))?;
    }
    fs::write(path, new_content).with_context(|| format!("escrevendo {}", path.display()))?;
    Ok(true)
}

fn read_json_opt(path: &Path) -> Result<Option<Value>> {
    match fs::read_to_string(path) {
        Ok(s) => Ok(Some(serde_json::from_str(&s).with_context(|| {
            format!("JSON inválido em {}", path.display())
        })?)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).with_context(|| format!("lendo {}", path.display())),
    }
}

/// Instala/atualiza `.claude/settings.json` no projeto, preservando o que
/// não é nosso. Retorna `true` se o arquivo foi (re)escrito.
pub fn install_hooks(project_dir: &Path, cfg: &HooksConfig) -> Result<bool> {
    let path = project_dir.join(".claude").join("settings.json");
    let merged = merge_settings(read_json_opt(&path)?, cfg);
    write_json_if_changed(&path, &merged)
}

/// Tira `server_name` de `disabledMcpjsonServers` em
/// `.claude/settings.local.json`, se estiver lá.
///
/// Devolve `true` quando removeu algo. Não cria o arquivo nem mexe em outras
/// chaves: se não há nada desabilitado, não há o que fazer.
pub fn enable_mcp_server(project_dir: &Path, server_name: &str) -> Result<bool> {
    let path = project_dir.join(".claude").join("settings.local.json");
    let Ok(texto) = fs::read_to_string(&path) else {
        return Ok(false);
    };
    let Ok(mut root) = serde_json::from_str::<Value>(&texto) else {
        return Ok(false);
    };
    let Some(obj) = root.as_object_mut() else {
        return Ok(false);
    };
    let Some(lista) = obj.get_mut("disabledMcpjsonServers").and_then(Value::as_array_mut) else {
        return Ok(false);
    };
    let antes = lista.len();
    lista.retain(|v| v.as_str() != Some(server_name));
    if lista.len() == antes {
        return Ok(false);
    }
    if lista.is_empty() {
        obj.remove("disabledMcpjsonServers");
    }
    write_json_if_changed(&path, &root)?;
    Ok(true)
}

/// Instala/atualiza `.mcp.json` no projeto, tocando só o servidor dado.
/// Retorna `true` se o arquivo foi (re)escrito.
pub fn install_mcp(
    project_dir: &Path,
    server_name: &str,
    command: &str,
    args: &[String],
) -> Result<bool> {
    let path = project_dir.join(".mcp.json");
    let merged = merge_mcp(read_json_opt(&path)?, server_name, command, args);
    write_json_if_changed(&path, &merged)
}

/// Resultado do [`setup_project`] — o que foi (re)escrito e onde, para quem
/// chama montar sua própria mensagem (CLI imprime, TUI põe no status).
#[derive(Debug, Clone)]
pub struct SetupOutcome {
    /// Arquivo de regras escrito em `.claude/orchestrator-rules.md`.
    pub rules_file: PathBuf,
    /// `true` se `.claude/settings.json` foi (re)escrito nesta chamada.
    pub hooks_written: bool,
    /// `true` se o servidor MCP estava desabilitado e foi reabilitado.
    pub mcp_reenabled: bool,
    /// `true` se `.mcp.json` foi (re)escrito nesta chamada.
    pub mcp_written: bool,
    /// Caminho resolvido do binário de hook.
    pub hook_bin: PathBuf,
    /// Caminho resolvido do binário MCP.
    pub mcp_bin: PathBuf,
    /// `false` se algum dos binários acima não existe no disco (aviso).
    pub binaries_present: bool,
}

/// Texto do arquivo de regras injetado no início da sessão do agente.
fn rules_text(project_name: &str) -> String {
    format!(
        "# Regras do Orchestrator — projeto {project_name}\n\n\
         Este projeto é gerenciado pelo Orchestrator e tem MEMÓRIA SEMÂNTICA\n\
         (ChromaDB + reranker): regras do dono deste projeto, memórias globais do\n\
         dono (valem em todo projeto) e memórias das IAs, cada uma com autor. A\n\
         cada prompt chega um índice do que importa para ele.\n\n\
         - Antes de alterar qualquer coisa, consulte a tool MCP `retrieve_memory`\n\
           (project: \"{project_name}\"): ferramentas que alteram o projeto ficam\n\
           bloqueadas pelo hook até a primeira consulta da sessão.\n\
         - Regras de segurança são IMPOSITIVAS: tool calls que as violem serão\n\
           bloqueadas pelo hook PreToolUse.\n\
         - Registre o que aprender com `store_memory` e decisões relevantes com\n\
           `log_decision`.\n\
         - Não sabe algo, ou dá para fazer ou testar melhor? Pesquise antes de\n\
           implementar, e compare arquiteturas em decisões de estrutura.\n"
    )
}

/// Instala hooks (`.claude/settings.json`), servidor MCP (`.mcp.json`) e o
/// arquivo de regras do Orchestrator em `project_dir` — idempotente
/// (write-if-changed, preserva chaves alheias).
///
/// Os binários `orchestrator-hook`/`orchestrator-mcp` são procurados ao lado
/// do executável atual (mesmo target dir / mesmo prefixo instalado). A
/// ausência deles NÃO é erro: devolve `binaries_present = false` para quem
/// chama avisar, já que a config aponta para o caminho esperado.
pub fn setup_project(project_dir: &Path, project_name: &str) -> Result<SetupOutcome> {
    let exe_dir = std::env::current_exe()
        .context("resolvendo executável atual")?
        .parent()
        .map(|p| p.to_path_buf())
        .context("executável sem diretório pai")?;
    let hook_bin = exe_dir.join("orchestrator-hook");
    let mcp_bin = exe_dir.join("orchestrator-mcp");

    let rules_file = project_dir.join(".claude").join("orchestrator-rules.md");
    if let Some(parent) = rules_file.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("criando diretório {}", parent.display()))?;
    }
    // Só reescreve as regras se mudaram (evita churn de mtime).
    let desired = rules_text(project_name);
    let rules_changed = fs::read_to_string(&rules_file).map(|c| c != desired).unwrap_or(true);
    if rules_changed {
        fs::write(&rules_file, &desired)
            .with_context(|| format!("escrevendo {}", rules_file.display()))?;
    }

    let hooks_written = install_hooks(
        project_dir,
        &HooksConfig {
            rules_file: rules_file.display().to_string(),
            orchestrator_hook_bin: hook_bin.display().to_string(),
        },
    )?;
    let mcp_written = install_mcp(project_dir, "orchestrator", &mcp_bin.display().to_string(), &[])?;
    // O `claude` pergunta uma vez se confia nos servidores MCP do projeto, e
    // um Enter distraído cai em "não usar" — o que grava o servidor em
    // `disabledMcpjsonServers` e deixa as CLIs sem as tools do Orchestrator
    // SEM avisar ninguém. Como o servidor é nosso e acabou de ser instalado,
    // tiramos essa marca.
    let mcp_reenabled = enable_mcp_server(project_dir, "orchestrator")?;

    let binaries_present = hook_bin.exists() && mcp_bin.exists();
    Ok(SetupOutcome {
        rules_file,
        hooks_written,
        mcp_written,
        mcp_reenabled,
        hook_bin,
        mcp_bin,
        binaries_present,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> HooksConfig {
        HooksConfig {
            rules_file: "/tmp/rules.md".into(),
            orchestrator_hook_bin: "/usr/local/bin/orch-hook".into(),
        }
    }

    #[test]
    fn hooks_value_shape() {
        let v = build_hooks_value(&cfg());
        assert_eq!(
            v["SessionStart"][0]["hooks"][0]["command"],
            "cat \"/tmp/rules.md\""
        );
        assert_eq!(
            v["PreToolUse"][0]["hooks"][0]["command"],
            "/usr/local/bin/orch-hook"
        );
        assert_eq!(v["PreToolUse"][0]["matcher"], "*");
        // O mesmo binário injeta o contexto de memória em todo prompt.
        assert_eq!(
            v["UserPromptSubmit"][0]["hooks"][0]["command"],
            "/usr/local/bin/orch-hook"
        );
    }

    #[test]
    fn merge_preserves_foreign_keys() {
        let existing = json!({
            "model": "opus",
            "hooks": {
                "PostToolUse": [{"hooks": [{"type": "command", "command": "echo done"}]}],
                "PreToolUse": [{"hooks": [{"type": "command", "command": "old"}]}]
            }
        });
        let merged = merge_settings(Some(existing), &cfg());
        // Chave alheia no topo preservada.
        assert_eq!(merged["model"], "opus");
        // Hook alheio preservado.
        assert_eq!(
            merged["hooks"]["PostToolUse"][0]["hooks"][0]["command"],
            "echo done"
        );
        // Nossa entrada substitui a antiga.
        assert_eq!(
            merged["hooks"]["PreToolUse"][0]["hooks"][0]["command"],
            "/usr/local/bin/orch-hook"
        );
        assert!(merged["hooks"]["SessionStart"].is_array());
        assert!(merged["hooks"]["UserPromptSubmit"].is_array());
    }

    #[test]
    fn merge_mcp_preserves_other_servers() {
        let existing = json!({
            "mcpServers": { "other": { "command": "other-srv", "args": [] } }
        });
        let merged = merge_mcp(
            Some(existing),
            "orchestrator",
            "orch-mcp",
            &["--port".to_owned(), "9000".to_owned()],
        );
        assert_eq!(merged["mcpServers"]["other"]["command"], "other-srv");
        assert_eq!(merged["mcpServers"]["orchestrator"]["command"], "orch-mcp");
        assert_eq!(merged["mcpServers"]["orchestrator"]["args"][1], "9000");
    }

    #[test]
    fn install_hooks_is_write_if_changed() {
        let dir = tempfile::tempdir().unwrap();
        let c = cfg();
        assert!(install_hooks(dir.path(), &c).unwrap()); // primeira escrita
        assert!(!install_hooks(dir.path(), &c).unwrap()); // sem mudança

        // Mudança de config reescreve.
        let c2 = HooksConfig {
            rules_file: "/tmp/other.md".into(),
            ..c
        };
        assert!(install_hooks(dir.path(), &c2).unwrap());

        // Conteúdo do usuário sobrevive a uma nova instalação.
        let path = dir.path().join(".claude/settings.json");
        let mut v: Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        v["permissions"] = json!({"allow": ["Bash(ls)"]});
        std::fs::write(&path, serde_json::to_string_pretty(&v).unwrap()).unwrap();
        assert!(install_hooks(dir.path(), &c2).is_ok());
        let after: Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(after["permissions"]["allow"][0], "Bash(ls)");
    }

    #[test]
    fn install_mcp_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        assert!(install_mcp(dir.path(), "orchestrator", "orch-mcp", &[]).unwrap());
        assert!(!install_mcp(dir.path(), "orchestrator", "orch-mcp", &[]).unwrap());
        let v: Value = serde_json::from_str(
            &std::fs::read_to_string(dir.path().join(".mcp.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(v["mcpServers"]["orchestrator"]["command"], "orch-mcp");
    }

    #[test]
    fn invalid_existing_json_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".mcp.json");
        std::fs::write(&path, "{ not json").unwrap();
        assert!(install_mcp(dir.path(), "s", "c", &[]).is_err());
    }
}

#[cfg(test)]
mod mcp_enable_tests {
    use super::*;

    fn escreve(dir: &Path, conteudo: &str) -> std::path::PathBuf {
        let p = dir.join(".claude").join("settings.local.json");
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(&p, conteudo).unwrap();
        p
    }

    #[test]
    fn removes_our_server_from_the_disabled_list() {
        // Caso real: um Enter no prompt de confiança do `claude` grava isto,
        // e as CLIs ficam sem as tools do Orchestrator sem ninguém perceber.
        let dir = tempfile::tempdir().unwrap();
        let p = escreve(
            dir.path(),
            r#"{"disabledMcpjsonServers":["orchestrator"],"outraChave":1}"#,
        );
        assert!(enable_mcp_server(dir.path(), "orchestrator").unwrap());
        let v: Value = serde_json::from_str(&fs::read_to_string(&p).unwrap()).unwrap();
        // A chave some quando fica vazia, e o resto do arquivo é preservado.
        assert!(v.get("disabledMcpjsonServers").is_none());
        assert_eq!(v["outraChave"], 1);
    }

    #[test]
    fn keeps_other_disabled_servers() {
        let dir = tempfile::tempdir().unwrap();
        let p = escreve(
            dir.path(),
            r#"{"disabledMcpjsonServers":["orchestrator","outro"]}"#,
        );
        assert!(enable_mcp_server(dir.path(), "orchestrator").unwrap());
        let v: Value = serde_json::from_str(&fs::read_to_string(&p).unwrap()).unwrap();
        assert_eq!(v["disabledMcpjsonServers"], serde_json::json!(["outro"]));
    }

    #[test]
    fn does_nothing_when_there_is_nothing_to_do() {
        let dir = tempfile::tempdir().unwrap();
        // Sem arquivo.
        assert!(!enable_mcp_server(dir.path(), "orchestrator").unwrap());
        // Com arquivo, mas sem a lista.
        escreve(dir.path(), r#"{"permissions":{}}"#);
        assert!(!enable_mcp_server(dir.path(), "orchestrator").unwrap());
        // Com a lista, mas sem o nosso servidor.
        escreve(dir.path(), r#"{"disabledMcpjsonServers":["outro"]}"#);
        assert!(!enable_mcp_server(dir.path(), "orchestrator").unwrap());
        // JSON quebrado não derruba o setup.
        escreve(dir.path(), "{quebrado");
        assert!(!enable_mcp_server(dir.path(), "orchestrator").unwrap());
    }
}

