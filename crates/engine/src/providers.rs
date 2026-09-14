//! Quem pode responder no chat agora.
//!
//! Cada provedor roda numa ferramenta (CLI oficial ou HTTP) com uma conta.
//! Antes de trocar, o usuário precisa saber se vai funcionar e, se não, o
//! que falta: instalar, entrar na conta ou definir a chave. Tudo aqui é
//! checagem local e barata (PATH, arquivo de credencial, variável), porque
//! roda toda vez que a lista abre.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use orchestrator_core::{LlmProvider, ProviderKind};

/// O estado de um provedor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Ready,
    NeedsLogin,
    NotInstalled,
    MissingKey,
}

impl State {
    pub fn glyph(self) -> &'static str {
        match self {
            State::Ready => "●",
            State::NeedsLogin => "○",
            State::NotInstalled | State::MissingKey => "✖",
        }
    }

    /// Código estável para a interface web.
    pub fn code(self) -> &'static str {
        match self {
            State::Ready => "pronto",
            State::NeedsLogin => "login",
            State::NotInstalled => "instalar",
            State::MissingKey => "chave",
        }
    }
}

/// Estado + o que fazer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Availability {
    pub state: State,
    /// O que falta, em uma frase (vazio quando pronto).
    pub hint: String,
    /// Comando que resolve dentro de um card (o login da CLI), quando existe.
    pub fix_command: Option<String>,
}

impl Availability {
    fn ready() -> Self {
        Self { state: State::Ready, hint: String::new(), fix_command: None }
    }

    pub fn is_ready(&self) -> bool {
        self.state == State::Ready
    }
}

/// O que se sabe da máquina: onde procurar binários, a pasta pessoal e as
/// variáveis de ambiente. Separado para os testes montarem uma máquina falsa.
#[derive(Debug, Clone, Default)]
pub struct Probe {
    pub path_dirs: Vec<PathBuf>,
    pub home: Option<PathBuf>,
    pub vars: HashMap<String, String>,
}

impl Probe {
    /// A máquina de verdade.
    pub fn current() -> Self {
        let vars: HashMap<String, String> = std::env::vars().collect();
        let home = vars.get("HOME").filter(|h| !h.is_empty()).map(PathBuf::from);
        let mut path_dirs: Vec<PathBuf> = std::env::var_os("PATH")
            .map(|p| std::env::split_paths(&p).collect())
            .unwrap_or_default();
        // Aberto pelo menu do desktop, o PATH pode não ter as pastas onde os
        // instaladores oficiais põem as CLIs.
        if let Some(h) = &home {
            for extra in [".local/bin", ".opencode/bin", ".npm-global/bin", "bin"] {
                let d = h.join(extra);
                if !path_dirs.contains(&d) {
                    path_dirs.push(d);
                }
            }
        }
        Self { path_dirs, home, vars }
    }

    /// Variável com valor (espaços não contam).
    pub fn var(&self, name: &str) -> Option<&str> {
        self.vars
            .get(name)
            .map(String::as_str)
            .filter(|v| !v.trim().is_empty())
    }

    /// Caminho completo do binário, se ele existir num dos diretórios.
    pub fn find_binary(&self, name: &str) -> Option<PathBuf> {
        let nomes: Vec<String> = if cfg!(windows) {
            vec![format!("{name}.exe"), format!("{name}.cmd"), name.to_string()]
        } else {
            vec![name.to_string()]
        };
        self.path_dirs
            .iter()
            .flat_map(|d| nomes.iter().map(move |n| d.join(n)))
            .find(|p| is_executable(p))
    }

    fn in_home(&self, rel: &str) -> Option<PathBuf> {
        self.home.as_ref().map(|h| h.join(rel))
    }

    /// `$XDG_DATA_HOME`, senão `~/.local/share`.
    fn data_dir(&self) -> Option<PathBuf> {
        self.var("XDG_DATA_HOME")
            .map(PathBuf::from)
            .or_else(|| self.in_home(".local/share"))
    }
}

fn is_executable(p: &Path) -> bool {
    let Ok(meta) = std::fs::metadata(p) else {
        return false;
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        meta.is_file() && meta.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        meta.is_file()
    }
}

/// Como instalar a ferramenta (instalador oficial de cada fornecedor).
pub fn install_command(kind: ProviderKind) -> &'static str {
    match kind {
        ProviderKind::ClaudeCli => "curl -fsSL https://claude.ai/install.sh | bash",
        ProviderKind::CodexCli => "npm i -g @openai/codex",
        ProviderKind::KimiCli => "curl -LsSf https://code.kimi.com/install.sh | bash",
        ProviderKind::AntigravityCli => "curl -fsSL https://antigravity.google/cli/install.sh | bash",
        ProviderKind::OpencodeCli => "npm i -g opencode-ai",
        ProviderKind::OpenAiCompat => "",
    }
}

/// O provedor funciona agora? Se não, o que falta.
pub fn availability(p: &LlmProvider, probe: &Probe) -> Availability {
    if let Some(bin) = p.kind.binary() {
        if probe.find_binary(bin).is_none() {
            return Availability {
                state: State::NotInstalled,
                hint: format!(
                    "{} não está instalado — instale com: `{}`",
                    p.kind.tool_label(),
                    install_command(p.kind)
                ),
                fix_command: None,
            };
        }
    }
    if let Err(var) = p.resolved_env(&|k| probe.var(k).map(str::to_string)) {
        return missing_key(&var);
    }
    match p.kind {
        ProviderKind::OpenAiCompat => {
            if !p.api_key_env.is_empty() && probe.var(&p.api_key_env).is_none() {
                return missing_key(&p.api_key_env);
            }
            Availability::ready()
        }
        // Outro endpoint pelo Claude Code: a chave (já conferida) basta.
        ProviderKind::ClaudeCli if !p.env.is_empty() => Availability::ready(),
        ProviderKind::ClaudeCli => {
            if claude_logged_in(probe) {
                Availability::ready()
            } else {
                login("entre na conta Anthropic: `claude auth login`", "claude auth login")
            }
        }
        ProviderKind::CodexCli => {
            if codex_logged_in(probe) {
                Availability::ready()
            } else {
                login("entre na conta do ChatGPT: `codex login`", "codex login")
            }
        }
        ProviderKind::KimiCli => {
            if kimi_logged_in(probe) {
                Availability::ready()
            } else {
                login("entre na conta Kimi: `kimi login`", "kimi login")
            }
        }
        ProviderKind::AntigravityCli => {
            if antigravity_logged_in(probe) {
                Availability::ready()
            } else {
                login("entre na conta Google: rode `agy` uma vez", "agy")
            }
        }
        ProviderKind::OpencodeCli => {
            let fornecedor = p.model.split('/').next().unwrap_or("");
            if (!p.api_key_env.is_empty() && probe.var(&p.api_key_env).is_some())
                || opencode_has_credential(probe, fornecedor)
            {
                Availability::ready()
            } else {
                let var = if p.api_key_env.is_empty() {
                    String::new()
                } else {
                    format!(" ou defina `{}`", p.api_key_env)
                };
                login(
                    &format!(
                        "falta a chave de {fornecedor} no OpenCode: `opencode auth login`{var}"
                    ),
                    "opencode auth login",
                )
            }
        }
    }
}

fn login(hint: &str, command: &str) -> Availability {
    Availability {
        state: State::NeedsLogin,
        hint: hint.to_string(),
        fix_command: Some(command.to_string()),
    }
}

fn missing_key(var: &str) -> Availability {
    Availability {
        state: State::MissingKey,
        hint: format!(
            "falta a chave: defina `{var}` (para o app aberto pelo menu, em \
             `~/.config/environment.d/orchestrator.conf`) e reabra o Orchestrator"
        ),
        fix_command: None,
    }
}

/// Claude Code: `~/.claude/.credentials.json` (ou `$CLAUDE_CONFIG_DIR`), ou
/// chave/token no ambiente. No macOS a credencial fica no Keychain, que não
/// dá para olhar daqui: lá o login é presumido.
fn claude_logged_in(probe: &Probe) -> bool {
    if cfg!(target_os = "macos")
        || probe.var("ANTHROPIC_API_KEY").is_some()
        || probe.var("CLAUDE_CODE_OAUTH_TOKEN").is_some()
    {
        return true;
    }
    let dir = probe
        .var("CLAUDE_CONFIG_DIR")
        .map(PathBuf::from)
        .or_else(|| probe.in_home(".claude"));
    dir.is_some_and(|d| d.join(".credentials.json").is_file())
}

/// Codex: `$CODEX_HOME/auth.json` (padrão `~/.codex`), ou `CODEX_API_KEY`.
fn codex_logged_in(probe: &Probe) -> bool {
    if probe.var("CODEX_API_KEY").is_some() {
        return true;
    }
    let dir = probe
        .var("CODEX_HOME")
        .map(PathBuf::from)
        .or_else(|| probe.in_home(".codex"));
    dir.is_some_and(|d| d.join("auth.json").is_file())
}

/// Kimi Code: credenciais em `~/.kimi/credentials/`.
fn kimi_logged_in(probe: &Probe) -> bool {
    probe
        .in_home(".kimi/credentials")
        .is_some_and(|d| dir_has_files(&d))
}

/// Antigravity: o login fica guardado em `~/.gemini/antigravity-cli/`.
fn antigravity_logged_in(probe: &Probe) -> bool {
    probe
        .in_home(".gemini/antigravity-cli")
        .is_some_and(|d| dir_has_files(&d.join("credentials")) || d.join("oauth_creds.json").is_file())
}

fn dir_has_files(dir: &Path) -> bool {
    std::fs::read_dir(dir)
        .map(|mut it| it.next().is_some())
        .unwrap_or(false)
}

/// OpenCode guarda as chaves em `<dados>/opencode/auth.json`, uma entrada
/// por fornecedor.
fn opencode_has_credential(probe: &Probe, fornecedor: &str) -> bool {
    if fornecedor.is_empty() {
        return false;
    }
    let Some(arquivo) = probe.data_dir().map(|d| d.join("opencode/auth.json")) else {
        return false;
    };
    std::fs::read_to_string(arquivo)
        .ok()
        .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
        .is_some_and(|v| v.get(fornecedor).is_some())
}

#[cfg(test)]
mod tests {
    use super::*;
    use orchestrator_core::Config;

    struct Maquina {
        _dir: tempfile::TempDir,
        probe: Probe,
    }

    /// Máquina falsa: pasta pessoal vazia, `bin` com os binários pedidos.
    fn maquina(binarios: &[&str]) -> Maquina {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        for b in binarios {
            let f = bin.join(b);
            std::fs::write(&f, "#!/bin/sh\n").unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&f, std::fs::Permissions::from_mode(0o755)).unwrap();
            }
        }
        let home = dir.path().join("home");
        std::fs::create_dir_all(&home).unwrap();
        Maquina {
            probe: Probe { path_dirs: vec![bin], home: Some(home), vars: HashMap::new() },
            _dir: dir,
        }
    }

    fn provedor(nome: &str) -> LlmProvider {
        Config::default()
            .llm_providers
            .into_iter()
            .find(|p| p.name == nome)
            .unwrap_or_else(|| panic!("sem provedor {nome}"))
    }

    fn escreve(probe: &Probe, rel: &str, conteudo: &str) {
        let f = probe.home.as_ref().unwrap().join(rel);
        std::fs::create_dir_all(f.parent().unwrap()).unwrap();
        std::fs::write(f, conteudo).unwrap();
    }

    #[test]
    fn missing_tool_says_how_to_install() {
        let m = maquina(&[]);
        let a = availability(&provedor("ChatGPT (Codex)"), &m.probe);
        assert_eq!(a.state, State::NotInstalled);
        assert!(a.hint.contains("npm i -g @openai/codex"), "{}", a.hint);
    }

    #[test]
    fn codex_needs_login_until_auth_file_exists() {
        let m = maquina(&["codex"]);
        let p = provedor("ChatGPT (Codex)");
        let a = availability(&p, &m.probe);
        assert_eq!(a.state, State::NeedsLogin);
        assert_eq!(a.fix_command.as_deref(), Some("codex login"));
        escreve(&m.probe, ".codex/auth.json", "{}");
        assert!(availability(&p, &m.probe).is_ready());
    }

    #[test]
    fn claude_login_is_the_credentials_file() {
        let m = maquina(&["claude"]);
        let p = provedor("Claude Code (CLI)");
        if !cfg!(target_os = "macos") {
            assert_eq!(availability(&p, &m.probe).state, State::NeedsLogin);
        }
        escreve(&m.probe, ".claude/.credentials.json", "{}");
        assert!(availability(&p, &m.probe).is_ready());
    }

    #[test]
    fn glm_through_claude_code_needs_only_the_key() {
        let mut m = maquina(&["claude"]);
        let p = provedor("GLM · Claude Code");
        let a = availability(&p, &m.probe);
        assert_eq!(a.state, State::MissingKey);
        assert!(a.hint.contains("ZHIPU_API_KEY"), "{}", a.hint);
        m.probe.vars.insert("ZHIPU_API_KEY".into(), "k".into());
        // Sem login Anthropic nenhum: a conta é a da Z.ai.
        assert!(availability(&p, &m.probe).is_ready());
    }

    #[test]
    fn opencode_accepts_env_key_or_its_own_credential() {
        let mut m = maquina(&["opencode"]);
        let p = provedor("Groq · OpenCode");
        let a = availability(&p, &m.probe);
        assert_eq!(a.state, State::NeedsLogin);
        assert!(a.hint.contains("groq") && a.hint.contains("GROQ_API_KEY"), "{}", a.hint);
        escreve(&m.probe, ".local/share/opencode/auth.json", r#"{"groq":{"type":"api","key":"x"}}"#);
        assert!(availability(&p, &m.probe).is_ready());
        // A credencial de outro fornecedor não serve.
        let glm = provedor("GLM · OpenCode");
        assert_eq!(availability(&glm, &m.probe).state, State::NeedsLogin);
        m.probe.vars.insert("ZHIPU_API_KEY".into(), "k".into());
        assert!(availability(&glm, &m.probe).is_ready());
    }

    #[test]
    fn http_provider_needs_its_key_and_local_ones_do_not() {
        let mut m = maquina(&[]);
        assert!(availability(&provedor("Ollama (local)"), &m.probe).is_ready());
        let groq = provedor("Groq");
        assert_eq!(availability(&groq, &m.probe).state, State::MissingKey);
        m.probe.vars.insert("GROQ_API_KEY".into(), "  ".into());
        assert_eq!(availability(&groq, &m.probe).state, State::MissingKey, "só espaços não é chave");
        m.probe.vars.insert("GROQ_API_KEY".into(), "gsk".into());
        assert!(availability(&groq, &m.probe).is_ready());
    }

    #[test]
    fn a_file_without_exec_permission_is_not_the_tool() {
        let m = maquina(&[]);
        let f = m.probe.path_dirs[0].join("kimi");
        std::fs::write(&f, "texto").unwrap();
        assert!(m.probe.find_binary("kimi").is_none() || cfg!(not(unix)));
    }
}
