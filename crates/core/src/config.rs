//! Configuração do Orchestrator.
//!
//! A configuração vive em JSON em `~/.config/orchestrator/config.json`
//! (resolvido via `directories::ProjectDirs("br", "sodre", "orchestrator")`).

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use directories::ProjectDirs;
use serde::{Deserialize, Serialize};

use crate::error::CoreError;

/// Backend de LLM usado pelo orquestrador para raciocínio próprio.
///
/// Apenas os tipos: nenhum código de rede vive neste crate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LlmBackend {
    /// API remota (ex.: Anthropic, OpenAI).
    Api { provider: String, model: String },
    /// Servidor Ollama local.
    Ollama { model: String, url: String },
    /// Reutiliza um CLI de agente como subprocesso.
    CliSubprocess,
}

impl Default for LlmBackend {
    fn default() -> Self {
        LlmBackend::CliSubprocess
    }
}

/// Um projeto gerenciado pelo Orchestrator.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectConfig {
    /// Nome único do projeto (usado como chave em comandos e na memória).
    pub name: String,
    /// Diretório raiz do projeto no disco.
    pub path: PathBuf,
    /// Objetivo de longo prazo do projeto, em linguagem natural.
    pub goal: String,
    /// CLI de agente a usar. Por enquanto apenas `"claude_code"`.
    #[serde(default = "default_cli")]
    pub cli: String,
}

fn default_cli() -> String {
    "claude_code".to_string()
}

/// Uma CLI de agente que o usuário pode abrir manualmente na TUI
/// (terminal PTY embutido — interação manual, nunca automação).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CliSpec {
    /// Nome exibido no seletor (ex.: "Claude Code").
    pub name: String,
    /// Comando a executar (ex.: "claude").
    pub command: String,
    /// Argumentos extras.
    #[serde(default)]
    pub args: Vec<String>,
    /// Variáveis da CLI (ex.: endpoint e chave para o Claude Code falar com
    /// GLM). `${VAR}` vem do ambiente do usuário.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
}

/// A ferramenta que roda o chat do orquestrador.
///
/// As CLIs usam a conta do próprio fornecedor (assinatura) e o fluxo nativo
/// dele, que gasta bem menos token do que mandar o mesmo trabalho por API
/// avulsa. O HTTP fica para quem só tem chave.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ProviderKind {
    /// Claude Code (`claude -p`, stream-json). Com `env`, o mesmo Claude
    /// Code fala com outro endpoint Anthropic-compatível (GLM, MiniMax).
    #[default]
    ClaudeCli,
    /// ChatGPT pelo Codex CLI (`codex exec --json`), conta do `codex login`.
    CodexCli,
    /// Kimi Code CLI (`kimi --print`), conta do `kimi login`.
    KimiCli,
    /// Google Antigravity CLI (`agy -p`), conta Google AI Pro/Ultra.
    AntigravityCli,
    /// OpenCode (`opencode run --format json`): GLM, MiniMax, Kimi, Groq…
    /// com a chave de cada fornecedor.
    OpencodeCli,
    /// Endpoint HTTP compatível com OpenAI (`{base_url}/chat/completions`).
    OpenAiCompat,
}

impl ProviderKind {
    /// Binário da ferramenta; `None` no chat HTTP.
    pub fn binary(self) -> Option<&'static str> {
        match self {
            ProviderKind::ClaudeCli => Some("claude"),
            ProviderKind::CodexCli => Some("codex"),
            ProviderKind::KimiCli => Some("kimi"),
            ProviderKind::AntigravityCli => Some("agy"),
            ProviderKind::OpencodeCli => Some("opencode"),
            ProviderKind::OpenAiCompat => None,
        }
    }

    /// Nome da ferramenta, como o usuário a conhece.
    pub fn tool_label(self) -> &'static str {
        match self {
            ProviderKind::ClaudeCli => "Claude Code",
            ProviderKind::CodexCli => "Codex",
            ProviderKind::KimiCli => "Kimi Code",
            ProviderKind::AntigravityCli => "Antigravity",
            ProviderKind::OpencodeCli => "OpenCode",
            ProviderKind::OpenAiCompat => "HTTP",
        }
    }
}

/// Um provedor de LLM que o chat do orquestrador pode usar.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LlmProvider {
    /// Nome exibido no seletor (ex.: "Groq").
    pub name: String,
    #[serde(default)]
    pub kind: ProviderKind,
    /// Base URL (só para `openai_compat`), ex.: "https://api.groq.com/openai/v1".
    #[serde(default)]
    pub base_url: String,
    /// Modelo. No HTTP, o do endpoint; no OpenCode, `fornecedor/modelo`; nas
    /// outras CLIs, vazio = o padrão da conta.
    #[serde(default)]
    pub model: String,
    /// Variável de ambiente com a chave (vazio = sem chave). Nas CLIs com
    /// conta própria fica vazio.
    #[serde(default)]
    pub api_key_env: String,
    /// Variáveis exportadas para a ferramenta (ex.: `ANTHROPIC_BASE_URL` para
    /// usar GLM pelo Claude Code). `${VAR}` no valor vira a variável do
    /// ambiente do usuário: a chave nunca fica gravada neste arquivo.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
    /// Chamada de ferramentas no chat HTTP. `None` segue o padrão de fábrica
    /// do provedor com este nome (e desligado quando não há).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tools: Option<bool>,
}

impl LlmProvider {
    /// O chat HTTP deste provedor oferece as ferramentas ao modelo?
    pub fn tools_enabled(&self) -> bool {
        self.tools.unwrap_or(false)
    }

    /// `env` com `${VAR}` resolvido. Erro: o nome da primeira variável sem
    /// valor.
    pub fn resolved_env(
        &self,
        lookup: &dyn Fn(&str) -> Option<String>,
    ) -> Result<Vec<(String, String)>, String> {
        self.env
            .iter()
            .map(|(k, v)| expand_vars(v, lookup).map(|v| (k.clone(), v)))
            .collect()
    }
}

/// Troca cada `${VAR}` de `value` pelo que `lookup` devolver. Variável sem
/// valor (ou só espaços) é erro, com o nome dela.
pub fn expand_vars(value: &str, lookup: &dyn Fn(&str) -> Option<String>) -> Result<String, String> {
    let mut out = String::new();
    let mut rest = value;
    while let Some(start) = rest.find("${") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        let Some(end) = after.find('}') else {
            out.push_str(&rest[start..]);
            return Ok(out);
        };
        let name = &after[..end];
        match lookup(name).filter(|v| !v.trim().is_empty()) {
            Some(v) => out.push_str(&v),
            None => return Err(name.to_string()),
        }
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    Ok(out)
}

fn default_llm_providers() -> Vec<LlmProvider> {
    let base = |name: &str, kind: ProviderKind| LlmProvider {
        name: name.into(),
        kind,
        base_url: String::new(),
        model: String::new(),
        api_key_env: String::new(),
        env: BTreeMap::new(),
        tools: None,
    };
    let compat = |name: &str, base_url: &str, model: &str, key: &str, tools: bool| LlmProvider {
        base_url: base_url.into(),
        model: model.into(),
        api_key_env: key.into(),
        tools: Some(tools),
        ..base(name, ProviderKind::OpenAiCompat)
    };
    // Claude Code apontado para o endpoint Anthropic de outro fornecedor. Os
    // apelidos (opus/sonnet/haiku) passam a significar os modelos dele.
    let claude_em = |name: &str, url: &str, key: &str, model: &str, small: &str| {
        let token = format!("${{{key}}}");
        let pares = [
            ("ANTHROPIC_BASE_URL", url),
            ("ANTHROPIC_AUTH_TOKEN", token.as_str()),
            ("ANTHROPIC_DEFAULT_OPUS_MODEL", model),
            ("ANTHROPIC_DEFAULT_SONNET_MODEL", model),
            ("ANTHROPIC_DEFAULT_HAIKU_MODEL", small),
        ];
        LlmProvider {
            api_key_env: key.into(),
            env: pares
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            ..base(name, ProviderKind::ClaudeCli)
        }
    };
    // Ids de fornecedor e modelo conferidos no catálogo que o OpenCode usa
    // (models.dev), todos com tool calling.
    let opencode = |name: &str, model: &str, key: &str| LlmProvider {
        model: model.into(),
        api_key_env: key.into(),
        ..base(name, ProviderKind::OpencodeCli)
    };
    vec![
        base("Claude Code (CLI)", ProviderKind::ClaudeCli),
        base("ChatGPT (Codex)", ProviderKind::CodexCli),
        base("Kimi Code", ProviderKind::KimiCli),
        base("Google (Antigravity)", ProviderKind::AntigravityCli),
        claude_em(
            "GLM · Claude Code",
            "https://api.z.ai/api/anthropic",
            "ZHIPU_API_KEY",
            "glm-5.3",
            "glm-5.3-flash",
        ),
        claude_em(
            "MiniMax · Claude Code",
            "https://api.minimax.io/anthropic",
            "MINIMAX_API_KEY",
            "MiniMax-M3",
            "MiniMax-M3",
        ),
        opencode("GLM · OpenCode", "zai-coding-plan/glm-5.3", "ZHIPU_API_KEY"),
        opencode("MiniMax · OpenCode", "minimax-coding-plan/MiniMax-M3", "MINIMAX_API_KEY"),
        opencode("Kimi · OpenCode", "kimi-for-coding/k3", "KIMI_API_KEY"),
        opencode("Groq · OpenCode", "groq/openai/gpt-oss-120b", "GROQ_API_KEY"),
        compat("Ollama (local)", "http://localhost:11434/v1", "llama3.2", "", true),
        compat("LM Studio (local)", "http://localhost:1234/v1", "local-model", "", false),
        compat(
            "Groq",
            "https://api.groq.com/openai/v1",
            "llama-3.3-70b-versatile",
            "GROQ_API_KEY",
            true,
        ),
        compat(
            "OpenRouter",
            "https://openrouter.ai/api/v1",
            "openrouter/auto",
            "OPENROUTER_API_KEY",
            true,
        ),
        compat("Perplexity", "https://api.perplexity.ai", "sonar", "PERPLEXITY_API_KEY", false),
        compat(
            "NVIDIA NIM",
            "https://integrate.api.nvidia.com/v1",
            "meta/llama-3.1-70b-instruct",
            "NVIDIA_API_KEY",
            true,
        ),
    ]
}

fn default_agent_clis() -> Vec<CliSpec> {
    let shell = std::env::var("SHELL").unwrap_or_else(|_| {
        if cfg!(windows) { "powershell".into() } else { "bash".into() }
    });
    let cli = |name: &str, command: &str| CliSpec {
        name: name.into(),
        command: command.into(),
        args: vec![],
        env: BTreeMap::new(),
    };
    // Claude Code noutro endpoint: as mesmas variáveis do provedor do chat.
    let claude_em = |name: &str, provedor: &str| CliSpec {
        env: default_llm_providers()
            .into_iter()
            .find(|p| p.name == provedor)
            .map(|p| p.env)
            .unwrap_or_default(),
        ..cli(name, "claude")
    };
    vec![
        cli("Claude Code", "claude"),
        cli("Codex CLI", "codex"),
        cli("Kimi Code", "kimi"),
        cli("Antigravity", "agy"),
        cli("OpenCode", "opencode"),
        claude_em("Claude Code · GLM", "GLM · Claude Code"),
        claude_em("Claude Code · MiniMax", "MiniMax · Claude Code"),
        cli("Gemini CLI", "gemini"),
        cli("Shell", &shell),
    ]
}

/// Defaults de custo-benefício para agentes disparados pelo orquestrador.
///
/// Campos vazios significam "usa o padrão da CLI". A TUI/o orquestrador
/// aplicam estes valores quando o usuário não escolhe explicitamente
/// (ex.: `/agente modelo=opus ...` sobrepõe o default).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct AgentDefaults {
    /// Modelo padrão (alias `haiku`/`sonnet`/`opus` ou nome completo).
    pub model: String,
    /// Fallback quando o modelo padrão está sobrecarregado.
    pub fallback_model: String,
    /// Nível de esforço padrão (`low`..`max`; vazio = padrão da CLI).
    pub effort: String,
    /// Modo de permissão padrão (`plan`, `auto`, `acceptEdits`, ...).
    pub permission_mode: String,
}

impl Default for AgentDefaults {
    fn default() -> Self {
        // sonnet com fallback haiku: bom equilíbrio custo/qualidade.
        AgentDefaults {
            model: "sonnet".into(),
            fallback_model: "haiku".into(),
            effort: String::new(),
            permission_mode: String::new(),
        }
    }
}

/// Configuração raiz do Orchestrator.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Projetos gerenciados.
    pub projects: Vec<ProjectConfig>,
    /// Caminho do banco de memória semântica (SQLite).
    pub memory_db_path: PathBuf,
    /// Backend de LLM do orquestrador.
    pub llm_backend: LlmBackend,
    /// CLIs de agente que o usuário pode abrir manualmente na TUI.
    pub agent_clis: Vec<CliSpec>,
    /// Provedores de LLM disponíveis para o chat do orquestrador.
    pub llm_providers: Vec<LlmProvider>,
    /// Defaults de modelo/effort/permission-mode dos agentes do orquestrador.
    pub agent_defaults: AgentDefaults,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            projects: Vec::new(),
            memory_db_path: default_memory_db_path(),
            llm_backend: LlmBackend::default(),
            agent_clis: default_agent_clis(),
            llm_providers: default_llm_providers(),
            agent_defaults: AgentDefaults::default(),
        }
    }
}

/// Diretórios padrão do aplicativo.
pub fn project_dirs() -> Result<ProjectDirs, CoreError> {
    ProjectDirs::from("br", "sodre", "orchestrator").ok_or(CoreError::NoProjectDirs)
}

/// Caminho padrão do arquivo de configuração
/// (`~/.config/orchestrator/config.json` no Linux).
pub fn default_config_path() -> Result<PathBuf, CoreError> {
    Ok(project_dirs()?.config_dir().join("config.json"))
}

/// Caminho padrão do banco de memória (no diretório de dados do usuário).
pub fn default_memory_db_path() -> PathBuf {
    project_dirs()
        .map(|d| d.data_dir().join("memory.db"))
        .unwrap_or_else(|_| PathBuf::from("memory.db"))
}

impl Config {
    /// Carrega a configuração de `path`. Se o arquivo não existir,
    /// retorna a configuração padrão (sem criá-lo).
    pub fn load(path: &Path) -> Result<Config, CoreError> {
        match fs::read_to_string(path) {
            Ok(text) => {
                let mut cfg: Config =
                    serde_json::from_str(&text).map_err(|source| CoreError::InvalidConfig {
                        path: path.to_path_buf(),
                        source,
                    })?;
                cfg.merge_default_providers();
                cfg.merge_default_clis();
                Ok(cfg)
            }
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(Config::default()),
            Err(source) => Err(CoreError::Io {
                path: path.to_path_buf(),
                source,
            }),
        }
    }

    /// Carrega do caminho padrão do usuário.
    pub fn load_default() -> Result<Config, CoreError> {
        Config::load(&default_config_path()?)
    }

    /// Grava a configuração em `path` (JSON identado), criando diretórios pai.
    pub fn save(&self, path: &Path) -> Result<(), CoreError> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|source| CoreError::Io {
                path: parent.to_path_buf(),
                source,
            })?;
        }
        let text = serde_json::to_string_pretty(self).expect("Config é sempre serializável");
        fs::write(path, text).map_err(|source| CoreError::Io {
            path: path.to_path_buf(),
            source,
        })
    }

    /// Traz os provedores de fábrica para uma configuração antiga, por nome:
    /// o que falta entra no fim da lista, e o que o usuário já tem fica como
    /// ele deixou (só ganha os campos novos que ainda não tinha).
    pub fn merge_default_providers(&mut self) {
        if self.llm_providers.is_empty() {
            return; // lista vazia de propósito: quem usa cai nos padrões.
        }
        for fabrica in default_llm_providers() {
            match self.llm_providers.iter_mut().find(|p| p.name == fabrica.name) {
                Some(p) => {
                    if p.tools.is_none() {
                        p.tools = fabrica.tools;
                    }
                }
                None => self.llm_providers.push(fabrica),
            }
        }
    }

    /// O mesmo para as CLIs de agente: as de fábrica que faltam entram no fim,
    /// por nome; as do usuário ficam como ele deixou.
    pub fn merge_default_clis(&mut self) {
        if self.agent_clis.is_empty() {
            return;
        }
        for fabrica in default_agent_clis() {
            if !self.agent_clis.iter().any(|c| c.name == fabrica.name) {
                self.agent_clis.push(fabrica);
            }
        }
    }

    /// Busca um projeto pelo nome.
    pub fn project(&self, name: &str) -> Result<&ProjectConfig, CoreError> {
        self.projects
            .iter()
            .find(|p| p.name == name)
            .ok_or_else(|| CoreError::UnknownProject(name.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Config {
        Config {
            projects: vec![ProjectConfig {
                name: "granafacil".into(),
                path: PathBuf::from("/tmp/granafacil"),
                goal: "app de finanças pessoais".into(),
                cli: default_cli(),
            }],
            memory_db_path: PathBuf::from("/tmp/memory.db"),
            llm_backend: LlmBackend::Ollama {
                model: "llama3".into(),
                url: "http://localhost:11434".into(),
            },
            agent_clis: default_agent_clis(),
            llm_providers: default_llm_providers(),
            agent_defaults: AgentDefaults::default(),
        }
    }

    #[test]
    fn roundtrip_json() {
        let cfg = sample();
        let text = serde_json::to_string(&cfg).unwrap();
        let back: Config = serde_json::from_str(&text).unwrap();
        assert_eq!(cfg, back);
    }

    #[test]
    fn save_and_load() {
        let dir = std::env::temp_dir().join(format!("orch-core-test-{}", std::process::id()));
        let path = dir.join("nested").join("config.json");
        let cfg = sample();
        cfg.save(&path).unwrap();
        let back = Config::load(&path).unwrap();
        assert_eq!(cfg, back);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_file_is_default() {
        let cfg = Config::load(Path::new("/definitely/not/here/config.json")).unwrap();
        assert!(cfg.projects.is_empty());
        assert_eq!(cfg.llm_backend, LlmBackend::CliSubprocess);
    }

    #[test]
    fn vars_expand_and_missing_ones_are_named() {
        let lookup = |k: &str| match k {
            "CHAVE" => Some("abc".to_string()),
            "VAZIA" => Some("  ".to_string()),
            _ => None,
        };
        assert_eq!(expand_vars("Bearer ${CHAVE}", &lookup).unwrap(), "Bearer abc");
        assert_eq!(expand_vars("sem variável", &lookup).unwrap(), "sem variável");
        assert_eq!(expand_vars("${NADA}", &lookup).unwrap_err(), "NADA");
        assert_eq!(expand_vars("${VAZIA}", &lookup).unwrap_err(), "VAZIA");
        // `${` sem fechar fica como está.
        assert_eq!(expand_vars("a ${b", &lookup).unwrap(), "a ${b");
    }

    #[test]
    fn claude_with_another_endpoint_keeps_the_key_out_of_the_file() {
        let glm = default_llm_providers()
            .into_iter()
            .find(|p| p.name == "GLM · Claude Code")
            .unwrap();
        assert_eq!(glm.kind, ProviderKind::ClaudeCli);
        assert_eq!(glm.env["ANTHROPIC_AUTH_TOKEN"], "${ZHIPU_API_KEY}");
        let env = glm
            .resolved_env(&|k| (k == "ZHIPU_API_KEY").then(|| "segredo".to_string()))
            .unwrap();
        assert!(env.contains(&("ANTHROPIC_AUTH_TOKEN".into(), "segredo".into())));
        assert_eq!(glm.resolved_env(&|_| None).unwrap_err(), "ZHIPU_API_KEY");
    }

    #[test]
    fn old_config_gains_the_new_providers_without_losing_edits() {
        let dir = tempfile_dir("merge");
        let path = dir.join("config.json");
        // Formato antigo: sem `env`/`tools`, Groq com modelo trocado pelo
        // usuário e um provedor dele que não existe de fábrica.
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            &path,
            r#"{"llm_providers":[
                {"name":"Claude Code (CLI)","kind":"claude_cli"},
                {"name":"Groq","kind":"open_ai_compat","base_url":"https://api.groq.com/openai/v1","model":"meu-modelo","api_key_env":"GROQ_API_KEY"},
                {"name":"Meu servidor","kind":"open_ai_compat","base_url":"http://x/v1","model":"m"}
            ]}"#,
        )
        .unwrap();
        let cfg = Config::load(&path).unwrap();
        let nomes: Vec<&str> = cfg.llm_providers.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(&nomes[..3], ["Claude Code (CLI)", "Groq", "Meu servidor"]);
        for novo in ["ChatGPT (Codex)", "Kimi Code", "Google (Antigravity)", "GLM · OpenCode"] {
            assert!(nomes.contains(&novo), "faltou {novo}: {nomes:?}");
        }
        assert_eq!(nomes.iter().filter(|n| **n == "Groq").count(), 1);
        let groq = cfg.llm_providers.iter().find(|p| p.name == "Groq").unwrap();
        assert_eq!(groq.model, "meu-modelo");
        assert!(groq.tools_enabled(), "Groq de fábrica tem tool calling");
        let meu = cfg.llm_providers.iter().find(|p| p.name == "Meu servidor").unwrap();
        assert!(!meu.tools_enabled());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn old_config_gains_the_new_clis_by_name() {
        let dir = tempfile_dir("clis");
        let path = dir.join("config.json");
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            &path,
            r#"{"agent_clis":[{"name":"Claude Code","command":"/opt/claude","args":["--verbose"]},{"name":"Shell","command":"fish"}]}"#,
        )
        .unwrap();
        let cfg = Config::load(&path).unwrap();
        let claude = cfg.agent_clis.iter().find(|c| c.name == "Claude Code").unwrap();
        assert_eq!(claude.command, "/opt/claude", "o que o usuário editou fica");
        for nova in ["Codex CLI", "Kimi Code", "Antigravity", "OpenCode", "Claude Code · GLM"] {
            assert!(cfg.agent_clis.iter().any(|c| c.name == nova), "faltou {nova}");
        }
        assert_eq!(cfg.agent_clis.iter().filter(|c| c.name == "Shell").count(), 1);
        let glm = cfg.agent_clis.iter().find(|c| c.name == "Claude Code · GLM").unwrap();
        assert_eq!(glm.env["ANTHROPIC_BASE_URL"], "https://api.z.ai/api/anthropic");
        let _ = fs::remove_dir_all(&dir);
    }

    fn tempfile_dir(nome: &str) -> PathBuf {
        std::env::temp_dir().join(format!("orch-core-{nome}-{}", std::process::id()))
    }

    #[test]
    fn unknown_project_errors() {
        let cfg = sample();
        assert!(cfg.project("granafacil").is_ok());
        assert!(matches!(
            cfg.project("nope"),
            Err(CoreError::UnknownProject(_))
        ));
    }
}
