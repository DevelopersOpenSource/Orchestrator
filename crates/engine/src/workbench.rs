//! O workbench sem interface: projetos, workspaces, CLIs reais, agentes,
//! chat do orquestrador, decisões e a fila de comandos do MCP.
//!
//! A TUI e o app desktop desenham este estado e chamam estes métodos — assim
//! um comando digitado em qualquer um dos dois faz exatamente a mesma coisa.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use orchestrator_cli_adapter::{discover, harness, AgentOptions, CliCapabilities};
use orchestrator_core::{AgentDefaults, CliSpec, Config, LlmProvider, ProviderKind};

use crate::providers::{self, Availability, Probe, State as ProviderState};
use orchestrator_memory::defaults;
use orchestrator_memory::store::{MemoryStore, PendingDecision};
use orchestrator_memory::{DecisionEntry, Memory};

use crate::agent_card::{AgentCard, AgentSpec};
use crate::chat::ChatState;
use crate::term::{self, CliState, PromptDelivery, TermSession};
use crate::{palette, picker_dir};

/// O que o núcleo pede a quem desenha: ele não conhece foco nem janela.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EngineEvent {
    /// Levar o foco para a grade de cards (uma CLI acabou de abrir).
    FocusGrid,
    /// Levar o foco para o chat (a workspace ficou sem cards).
    FocusChat,
    /// Mostrar a escolha de modelo do chat (`/modelo` sem nome).
    ChooseModel,
    /// Mostrar a lista de provedores do chat (`/provedor` sem nome).
    ChooseProvider,
    /// Abrir o manual (`/ajuda`).
    ShowHelp,
}

/// Como abrir uma CLI num card (ver [`Engine::card_launch`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CardLaunch {
    pub program: String,
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
}

/// O que aconteceu com o texto entregue a [`Engine::run_command`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandOutcome {
    /// Era comando e já rodou: o resultado está em `status`, no chat ou em
    /// [`EngineEvent`]s.
    Done,
    /// Não é comando: a interface manda como mensagem ao orquestrador.
    NotCommand,
}

pub const RELOAD_EVERY: Duration = Duration::from_secs(2);

/// Workspaces disponíveis (Alt+↑/↓ alterna).
pub const WS_COUNT: usize = 4;

/// Máximo de cards por workspace.
pub const MAX_PANES: usize = 8;

/// Teto padrão de iterações autônomas por agente (proteção de custo).
/// Intervalo de publicação do estado das CLIs no banco (o MCP lê ali).
pub const CLI_SYNC_EVERY: Duration = Duration::from_secs(1);

/// Teto de notificações automáticas de conclusão por execução da TUI —
/// proteção de custo: cada notificação é um turno do orquestrador.
pub const MAX_AUTO_NOTIFICATIONS: usize = 60;

/// Linhas de tela guardadas no estado publicado (`cli_state`) a cada ciclo.
pub const NOTIFY_TAIL_LINES: usize = 24;

/// Tamanho do resumo de uma linha na notificação de conclusão.
pub const NOTIFY_SUMMARY_CHARS: usize = 120;

/// Teto de trechos que `cli_read` devolve numa busca.
pub const SEARCH_MAX_HITS: usize = 10;

/// Quantas linhas do transcript são restauradas ao abrir a TUI.
pub const CHAT_RESTORE_LINES: usize = 200;

/// Entradas de auditoria mostradas como "progresso recente".
pub const PROGRESS_LINES: usize = 3;

/// Primeira opção do seletor de modelo: devolve a escolha ao provedor.
pub const DEFAULT_MODEL_LABEL: &str = "padrão do provedor";

pub const DEFAULT_MAX_ITERATIONS: usize = 6;

/// Teto absoluto de iterações autônomas, mesmo se o usuário pedir mais.
pub const MAX_ITERATIONS_CAP: usize = 50;

/// Resultado de [`App::parse_agent_flags`]: opções + autopilot + tarefa.
pub struct ParsedAgent {
    pub options: AgentOptions,
    pub auto: bool,
    pub max_iterations: usize,
    /// O usuário deu `mode=`/`perm=` explícito? Se sim, a postura não mexe no
    /// modo de permissão (precedência do explícito sobre a postura).
    pub explicit_mode: bool,
    pub task: String,
}

/// Postura de permissão aplicada à PRÓXIMA `/agente` spawnada. Shift+Tab no
/// chat cicla entre as três — "vai do gosto pessoal de cada um".
///
/// O gate de segurança (hook `PreToolUse` + regras `deny`/`ask` da memória)
/// roda SEMPRE, independentemente da postura; o que muda é o que acontece com
/// as tool calls *limpas* (sem regra aplicável):
/// - [`BypassHook`]: autônomo — o hook libera as limpas explicitamente
///   (`ORCHESTRATOR_AUTONOMOUS=1`), nada trava no prompt nativo do `claude`.
/// - [`AcceptEdits`]: `--permission-mode acceptEdits` (edições sem perguntar,
///   demais ações conforme o `claude`).
/// - [`AskPerTool`]: `--permission-prompt-tool` delega cada permissão à tool
///   MCP `permission_prompt`, que pausa esperando a decisão do usuário.
///   Builds do `claude` SEM essa flag (removida nas recentes) caem para o
///   gate autônomo do hook, com aviso no status — headless sem canal de
///   aprovação travaria o turno.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentPosture {
    BypassHook,
    AcceptEdits,
    AskPerTool,
}

impl AgentPosture {
    /// Próxima postura no ciclo (Shift+Tab).
    pub fn next(self) -> Self {
        match self {
            AgentPosture::BypassHook => AgentPosture::AcceptEdits,
            AgentPosture::AcceptEdits => AgentPosture::AskPerTool,
            AgentPosture::AskPerTool => AgentPosture::BypassHook,
        }
    }

    /// Rótulo curto para título/rodapé.
    pub fn label(self) -> &'static str {
        match self {
            AgentPosture::BypassHook => "autônomo",
            AgentPosture::AcceptEdits => "aceita-edições",
            AgentPosture::AskPerTool => "pergunta-por-tool",
        }
    }

    /// Exporta `ORCHESTRATOR_AUTONOMOUS=1` (hook é a autoridade)?
    pub fn autonomous(self) -> bool {
        matches!(self, AgentPosture::BypassHook)
    }

    /// Aplica a postura às opções do agente (quando o usuário não deu `mode=`
    /// explícito). Tudo ainda é filtrado por `caps.supports(...)` em `to_args`.
    pub fn apply(self, opts: &mut AgentOptions) {
        match self {
            // Hook autoritativo via env; sem flag de permission-mode.
            AgentPosture::BypassHook => {}
            AgentPosture::AcceptEdits => {
                opts.permission_mode = Some("acceptEdits".into());
            }
            AgentPosture::AskPerTool => {
                opts.permission_prompt_tool =
                    Some("mcp__orchestrator__permission_prompt".into());
            }
        }
    }
}

/// Um card da grade: terminal manual ou agente do orquestrador.
/// Um card da grade. `AgentCard` é bem maior que `TermSession`, então vai
/// em `Box` para o enum não carregar o tamanho do maior em cada elemento.
pub enum Pane {
    Term(Box<TermSession>),
    Agent(Box<AgentCard>),
}

#[derive(Default)]
pub struct Workspace {
    pub panes: Vec<Pane>,
    pub focused: usize,
    /// Pasta desta workspace (`/pasta <caminho>`). Vazia = usa a pasta do
    /// projeto ativo. CLIs e agentes abertos aqui herdam este diretório.
    pub dir: Option<PathBuf>,
    /// O chat DESTA workspace, guardado enquanto ela não está ativa.
    ///
    /// Um chat só para tudo misturava contextos: conversar sobre o backend na
    /// workspace 1 e sobre o site na 2 levava o orquestrador a responder com
    /// a memória errada. Aqui cada workspace tem conversa, sessão e fila
    /// próprias; trocar de workspace troca o chat junto (`swap`), então uma
    /// resposta em andamento continua chegando na workspace de origem.
    pub chat: Option<ChatState>,
}

pub struct Engine {
    pub store: MemoryStore,
    // --- workbench ---
    pub chat: ChatState,
    /// As workspaces do projeto ATIVO agora — CLIs abertas, terminais,
    /// agentes. Trocar de projeto arquiva este vetor inteiro em
    /// `workspaces_by_project` (pelo nome do projeto que está saindo) e traz
    /// de volta o do projeto novo, ou cria vazio se for a primeira vez nele.
    /// Sem isso, os cards de um projeto continuavam visíveis e comandáveis
    /// depois de trocar para outro — misturava CLI de projeto errado.
    pub workspaces: Vec<Workspace>,
    /// Workspaces dos OUTROS projetos, arquivadas enquanto eles não estão
    /// ativos. Os processos (PTY, threads dos agentes) continuam vivos —
    /// só saem da tela; voltam do jeito que ficaram ao trocar de volta.
    workspaces_by_project: std::collections::HashMap<String, Vec<Workspace>>,
    pub ws_idx: usize,
    pub term_counter: usize,
    pub agent_counter: usize,
    pub clis: Vec<CliSpec>,
    pub providers: Vec<LlmProvider>,
    pub projects: Vec<String>,
    pub project_paths: Vec<PathBuf>,
    /// Meta de cada projeto (paralelo a `projects`) — guia o loop autônomo.
    pub project_goals: Vec<String>,
    pub project_idx: usize,
    pub pending: Vec<PendingDecision>,
    pub memories: Vec<Memory>,
    pub audit: Vec<DecisionEntry>,
    /// Ferramentas que o orquestrador executou (sandbox/navegador).
    pub tool_calls: Vec<(String, String, String, bool, String)>,
    pub status: String,
    pub last_reload: Instant,
    /// Defaults de custo-benefício aplicados aos agentes (config).
    pub agent_defaults: AgentDefaults,
    /// Capacidades da CLI de agente padrão (`claude`), descobertas do help.
    pub caps: CliCapabilities,
    /// Postura de permissão da próxima `/agente` (Shift+Tab cicla).
    pub posture: AgentPosture,
    /// Caminho do banco de memória — exportado aos agentes (`ORCHESTRATOR_DB`).
    pub db_path: PathBuf,
    /// Ids de decisões pendentes já anunciadas no chat (evita repetir).
    pub seen_decisions: std::collections::HashSet<String>,
    /// Última publicação do estado das CLIs no banco.
    pub last_cli_sync: Instant,
    /// Notificações automáticas de conclusão já enviadas (teto de custo).
    pub auto_notifications: usize,
    /// Notificar o orquestrador quando uma CLI concluir? (`/auto off`).
    pub notify_on_done: bool,
    /// Sessão do chat já persistida (evita gravar a cada tick).
    pub saved_session: Option<String>,
    /// Modelo escolhido para o chat (vazio = padrão do provedor/CLI).
    /// Persistido por provedor em `ui_state`.
    pub chat_model: String,
    /// Avisos do estado atual (mostrados no manual, não no rodapé).
    pub notices: Vec<String>,
    /// Configuração carregada, para gravar projetos novos de volta nela.
    pub config: Config,
    /// Onde a configuração mora (None = não dá para persistir projeto novo).
    pub config_path: Option<PathBuf>,
    /// Pedidos à interface (foco etc.), drenados por quem desenha.
    events: Vec<EngineEvent>,
    /// Modelos ao vivo por provedor (`GET /models` da API dele), pelo nome do
    /// provedor. Só entra aqui quando alguém abre o seletor — nada busca
    /// sozinho no fundo.
    pub live_models: std::collections::HashMap<String, Vec<String>>,
    /// Nome do provedor com um pedido de modelos em andamento agora — evita
    /// empilhar outra busca em cima de uma que já está a caminho.
    models_loading: Option<String>,
    models_tx: std::sync::mpsc::Sender<(String, Result<Vec<String>, String>)>,
    models_rx: std::sync::mpsc::Receiver<(String, Result<Vec<String>, String>)>,
    /// Resultado do último teste de modelo (`/provedor/modelo` → texto ou
    /// erro), para o seletor mostrar ✔/✖ sem travar a interface.
    pub last_model_test: Option<(String, Result<String, String>)>,
    model_test_tx: std::sync::mpsc::Sender<(String, Result<String, String>)>,
    model_test_rx: std::sync::mpsc::Receiver<(String, Result<String, String>)>,
}

impl Engine {
    pub fn new(store: MemoryStore, config: &Config, db_path: PathBuf) -> Self {
        let mut projects: Vec<String> = config.projects.iter().map(|p| p.name.clone()).collect();
        let mut project_paths: Vec<PathBuf> =
            config.projects.iter().map(|p| p.path.clone()).collect();
        let mut project_goals: Vec<String> =
            config.projects.iter().map(|p| p.goal.clone()).collect();
        // Sem projeto na configuração, a pasta de onde a CLI foi chamada
        // VIRA o projeto — com nome e descrição tirados dela mesma. Chamar
        // isso de "default" perdia a informação e fazia a adoção achar que
        // já existia um projeto para este diretório.
        if projects.is_empty() {
            let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
            let cwd = cwd.canonicalize().unwrap_or(cwd);
            let hint = orchestrator_core::detect::describe_project(&cwd);
            projects.push(hint.name);
            project_paths.push(cwd);
            project_goals.push(hint.summary);
        }
        let providers = if config.llm_providers.is_empty() {
            Config::default().llm_providers
        } else {
            config.llm_providers.clone()
        };
        let (models_tx, models_rx) = std::sync::mpsc::channel();
        let (model_test_tx, model_test_rx) = std::sync::mpsc::channel();
        let mut app = Self {
            store,
            chat: ChatState::new(providers[0].clone()),
            workspaces: (0..WS_COUNT).map(|_| Workspace::default()).collect(),
            workspaces_by_project: std::collections::HashMap::new(),
            ws_idx: 0,
            term_counter: 0,
            agent_counter: 0,
            clis: config.agent_clis.clone(),
            providers,
            projects,
            project_paths,
            project_goals,
            project_idx: 0,
            pending: Vec::new(),
            memories: Vec::new(),
            audit: Vec::new(),
            tool_calls: Vec::new(),
            status: String::new(),
            last_reload: Instant::now() - RELOAD_EVERY,
            agent_defaults: config.agent_defaults.clone(),
            caps: discover("claude"),
            posture: AgentPosture::BypassHook,
            db_path,
            seen_decisions: std::collections::HashSet::new(),
            last_cli_sync: Instant::now() - CLI_SYNC_EVERY,
            auto_notifications: 0,
            notify_on_done: true,
            saved_session: None,
            chat_model: String::new(),
            notices: Vec::new(),
            config: config.clone(),
            config_path: None,
            events: Vec::new(),
            live_models: std::collections::HashMap::new(),
            models_loading: None,
            models_tx,
            models_rx,
            last_model_test: None,
            model_test_tx,
            model_test_rx,
        };
        // A ordem importa: `restore_chat` SUBSTITUI o transcript, então ele
        // vem primeiro — anunciar antes faria o alerta de decisão pendente
        // ser apagado pela restauração (e, com o id já em `seen_decisions`,
        // ele nunca mais reapareceria).
        // O provedor escolhido da última vez (se ainda existir na config).
        if let Ok(Some(nome)) = app.store.ui_get(CHAT_PROVIDER_KEY) {
            if let Some(p) = app.providers.iter().find(|p| p.name == nome).cloned() {
                app.chat.provider = p;
            }
        }
        app.restore_chat();
        app.reload();
        app
    }

    /// Restaura a conversa e a sessão do chat do projeto ativo (persistidas
    /// em `chat_transcript`/`ui_state` pela execução anterior da TUI).
    pub fn restore_chat(&mut self) {
        let project = self.project().to_string();
        // As CLIs da execução anterior morreram com a TUI; sem limpar, o
        // orquestrador voltaria vendo cards que não existem mais. Mas a
        // COMPOSIÇÃO fica guardada: dizemos o que esta workspace tinha.
        let _ = self.store.clear_cli_states(&project);
        let tinha = self.saved_workspace_clis();
        if !tinha.is_empty() && self.workspaces[self.ws_idx].panes.is_empty() {
            self.status = format!(
                "workspace {} tinha: {} — peça ao orquestrador para reabrir, ou /cli <nome>",
                self.ws_idx + 1,
                tinha.join(", ")
            );
        }
        let ws = self.ws_idx;
        let lines = self
            .store
            .recent_chat_lines(&project, ws, CHAT_RESTORE_LINES)
            .unwrap_or_default();
        let session = self
            .store
            .ui_get(&provider_session_key(&project, ws, &self.chat.provider.name))
            .ok()
            .flatten();
        let restored = !lines.is_empty();
        // Pastas por workspace escolhidas em execuções anteriores.
        for i in 0..WS_COUNT {
            if let Ok(Some(dir)) = self.store.ui_get(&format!("ws.{i}.dir")) {
                let path = PathBuf::from(dir);
                if path.is_dir() {
                    self.workspaces[i].dir = Some(path);
                }
            }
        }
        // Modelo escolhido antes para o provedor ativo.
        self.chat_model = self
            .store
            .ui_get(&format!("chat.model.{}", self.chat.provider.name))
            .ok()
            .flatten()
            .unwrap_or_default();
        self.saved_session = session.clone();
        self.chat.restore(lines, session);
        // Transcript novo: reanuncia o que ainda estiver pendente.
        self.seen_decisions.clear();
        if restored {
            self.status = format!(
                "sessão de \"{project}\" restaurada — ↑ recupera mensagens, /nova começa do zero."
            );
        }
    }

    /// Persiste o que o chat produziu neste tick (transcript + sessão).
    pub fn persist_chat(&mut self) {
        let project = self.project().to_string();
        let ws = self.ws_idx;
        for (who, text) in self.chat.take_persist() {
            let _ = self.store.append_chat_line(&project, ws, &who, &text);
        }
        if self.chat.session_id != self.saved_session {
            if let Some(id) = self.chat.session_id.clone() {
                let key = provider_session_key(&project, ws, &self.chat.provider.name);
                let _ = self.store.ui_set(&key, &id);
                self.saved_session = Some(id);
            }
        }
    }

    pub fn project(&self) -> &str {
        &self.projects[self.project_idx]
    }

    pub fn project_path(&self) -> PathBuf {
        self.project_paths
            .get(self.project_idx)
            .cloned()
            .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")))
    }

    /// Pasta onde abrir CLIs/agentes da workspace atual: a pasta da
    /// workspace, se definida (`/pasta`), senão a do projeto.
    pub fn workspace_dir(&self) -> PathBuf {
        self.workspaces
            .get(self.ws_idx)
            .and_then(|w| w.dir.clone())
            .unwrap_or_else(|| self.project_path())
    }

    /// Adota a pasta atual como projeto quando nenhuma configuração a cobre.
    ///
    /// Abrir a CLI numa pasta deveria bastar para começar: sem cadastrar
    /// projeto, sem escolher caminho, sem abrir outra conversa. Se o diretório
    /// de onde o Orchestrator foi chamado não corresponde a nenhum projeto
    /// conhecido, olhamos de que a pasta se trata
    /// ([`orchestrator_core::detect`]) e a adotamos — gravando na
    /// configuração, para na próxima vez já estar lá.
    ///
    /// Devolve a frase de apresentação quando adotou algo novo.
    pub fn adopt_current_dir(&mut self) -> Option<String> {
        let cwd = std::env::current_dir().ok()?;
        let cwd = cwd.canonicalize().unwrap_or(cwd);
        // Já existe projeto apontando para cá? Então só ativa ele.
        let existente = self.project_paths.iter().position(|p| {
            p.canonicalize().map(|c| c == cwd).unwrap_or(false)
        });
        if let Some(i) = existente {
            if i != self.project_idx {
                self.project_idx = i;
                self.reload();
                self.restore_chat();
            }
            // O projeto pode ter nascido do fallback (pasta sem configuração
            // nenhuma): nesse caso ele ainda não existe no arquivo, e grava-lo
            // agora é o que faz a próxima abertura já conhecer a pasta.
            let no_arquivo = self
                .config
                .projects
                .iter()
                .any(|p| p.path.canonicalize().map(|c| c == cwd).unwrap_or(false));
            if no_arquivo {
                return None;
            }
            let hint = orchestrator_core::detect::describe_project(&cwd);
            self.config.projects.push(orchestrator_core::ProjectConfig {
                name: self.projects[i].clone(),
                path: cwd.clone(),
                goal: hint.summary.clone(),
                cli: "claude_code".to_string(),
            });
            if let Some(cp) = self.config_path.clone() {
                if let Err(e) = self.config.save(&cp) {
                    self.notices
                        .push(format!("projeto adotado só nesta sessão: {e}"));
                }
            }
            self.project_goals[i] = hint.summary.clone();
            self.workspaces[0].dir = Some(cwd.clone());
            let _ = self.store.ui_set("ws.0.dir", &cwd.display().to_string());
            return Some(hint.describe());
        }
        // Pasta sem projeto: descobre do que se trata e adota.
        let hint = orchestrator_core::detect::describe_project(&cwd);
        let nome = self.nome_livre(&hint.name);
        self.config.projects.push(orchestrator_core::ProjectConfig {
            name: nome.clone(),
            path: cwd.clone(),
            goal: hint.summary.clone(),
            cli: "claude_code".to_string(),
        });
        if let Some(cp) = self.config_path.clone() {
            if let Err(e) = self.config.save(&cp) {
                self.notices
                    .push(format!("projeto adotado só nesta sessão: {e}"));
            }
        }
        self.projects.push(nome.clone());
        self.project_paths.push(cwd.clone());
        self.project_goals.push(hint.summary.clone());
        self.project_idx = self.projects.len() - 1;
        // A workspace 1 já aponta para a pasta adotada.
        self.workspaces[0].dir = Some(cwd.clone());
        let _ = self.store.ui_set("ws.0.dir", &cwd.display().to_string());
        self.reload();
        self.restore_chat();
        Some(hint.describe())
    }

    /// Nome ainda não usado, acrescentando sufixo se preciso.
    pub fn nome_livre(&self, desejado: &str) -> String {
        let base = if desejado.trim().is_empty() {
            "projeto".to_string()
        } else {
            desejado.trim().to_string()
        };
        if !self.projects.iter().any(|p| p.eq_ignore_ascii_case(&base)) {
            return base;
        }
        (2..99)
            .map(|n| format!("{base}-{n}"))
            .find(|c| !self.projects.iter().any(|p| p.eq_ignore_ascii_case(c)))
            .unwrap_or(base)
    }

    /// Cria um projeto novo e passa a trabalhar nele.
    ///
    /// Sem caminho, abre o seletor de pastas do sistema — digitar caminho na
    /// mão é onde mais se erra, e não havia como criar projeto pela TUI.
    pub fn create_project(&mut self, raw: &str) {
        let raw = raw.trim();
        let (nome, caminho) = match raw.split_once(char::is_whitespace) {
            Some((n, c)) => (n.trim(), c.trim().to_string()),
            None => (raw, String::new()),
        };
        if nome.is_empty() {
            self.status = "uso: /novo-projeto <nome> [caminho] — sem caminho abre o seletor".into();
            return;
        }
        if self.projects.iter().any(|p| p.eq_ignore_ascii_case(nome)) {
            self.status = format!("já existe um projeto chamado \"{nome}\" — /projeto {nome} abre");
            return;
        }
        let pasta = if caminho.is_empty() {
            match picker_dir::pick_directory(
                &format!("Pasta do projeto \"{nome}\""),
                &self.workspace_dir(),
            ) {
                Ok(p) => p,
                Err(e) => {
                    self.status = format!("projeto não criado: {}", e.message());
                    return;
                }
            }
        } else {
            expand_tilde(&caminho)
        };
        if !pasta.is_dir() {
            if let Err(e) = std::fs::create_dir_all(&pasta) {
                self.status = format!("não consegui criar {}: {e}", pasta.display());
                return;
            }
        }
        let pasta = pasta.canonicalize().unwrap_or(pasta);

        self.config.projects.push(orchestrator_core::ProjectConfig {
            name: nome.to_string(),
            path: pasta.clone(),
            goal: String::new(),
            cli: "claude_code".to_string(),
        });
        let gravado = match &self.config_path {
            Some(cp) => match self.config.save(cp) {
                Ok(()) => true,
                Err(e) => {
                    self.notices.push(format!("projeto criado só nesta sessão: {e}"));
                    false
                }
            },
            None => {
                self.notices.push(
                    "projeto criado só nesta sessão (não sei onde fica o config.json)".into(),
                );
                false
            }
        };
        self.projects.push(nome.to_string());
        self.project_paths.push(pasta.clone());
        self.project_goals.push(String::new());
        self.switch_project(Some(nome));
        self.status = format!(
            "projeto \"{nome}\" criado em {}{}",
            short_path(&pasta),
            if gravado { "" } else { " (só nesta sessão)" }
        );
    }

    /// Define a pasta da workspace atual (`/pasta <caminho>`), expandindo `~`.
    /// Persistida em `ui_state` para voltar na próxima execução.
    pub fn set_workspace_dir(&mut self, raw: &str) {
        self.set_workspace_dir_at(self.ws_idx, raw);
    }

    /// Como [`Self::set_workspace_dir`], para qualquer workspace do projeto
    /// ativo (o app muda a pasta de uma workspace pelo menu dela, sem
    /// precisar entrar nela). CLIs já abertas seguem onde estão; as novas,
    /// a sandbox e a IDE passam a usar a pasta nova.
    pub fn set_workspace_dir_at(&mut self, idx: usize, raw: &str) {
        if idx >= self.workspaces.len() {
            return;
        }
        let raw = raw.trim();
        let key = format!("ws.{idx}.dir");
        // Sem argumento: abre o explorador do sistema.
        if raw.is_empty() {
            let atual = self.workspaces[idx].dir.clone().unwrap_or_else(|| self.project_path());
            match picker_dir::pick_directory(&format!("Pasta da workspace {}", idx + 1), &atual) {
                Ok(p) => {
                    let escolhido = p.display().to_string();
                    return self.set_workspace_dir_at(idx, &escolhido);
                }
                Err(e) => {
                    self.status = format!(
                        "{} (use `/pasta <caminho>` ou `/pasta -` para voltar à do projeto)",
                        e.message()
                    );
                    return;
                }
            }
        }
        if raw == "-" {
            self.workspaces[idx].dir = None;
            let _ = self.store.ui_delete(&key);
            self.status = format!(
                "workspace {} volta a usar a pasta do projeto ({})",
                idx + 1,
                self.project_path().display()
            );
            return;
        }
        let expanded = expand_tilde(raw);
        if !expanded.is_dir() {
            self.status = format!("pasta não encontrada: {}", expanded.display());
            return;
        }
        let canonical = expanded.canonicalize().unwrap_or(expanded);
        let _ = self.store.ui_set(&key, &canonical.display().to_string());
        self.workspaces[idx].dir = Some(canonical.clone());
        if idx == self.ws_idx {
            self.ensure_project_setup();
        }
        self.status = format!("workspace {} agora abre CLIs em {}", idx + 1, canonical.display());
    }

    /// Muda a pasta de um projeto (menu do projeto no app, `/pasta-projeto`).
    ///
    /// Nada que já existe é perdido: conversa, memória e decisões são do
    /// projeto pelo NOME, não pela pasta; CLIs já abertas continuam rodando
    /// onde nasceram. Workspaces sem pasta própria passam a seguir a nova —
    /// CLIs novas, sandbox e IDE já abrem nela.
    pub fn set_project_path(&mut self, name: &str, raw: &str) {
        let Some(idx) = self.projects.iter().position(|p| p.eq_ignore_ascii_case(name.trim())) else {
            self.status = format!("projeto \"{}\" não existe", name.trim());
            return;
        };
        if raw.trim().is_empty() {
            let atual = self.project_paths[idx].clone();
            match picker_dir::pick_directory(&format!("Pasta do projeto {}", self.projects[idx]), &atual) {
                Ok(p) => return self.set_project_path(name, &p.display().to_string()),
                Err(e) => {
                    self.status = format!("{} (use `/pasta-projeto <caminho>`)", e.message());
                    return;
                }
            }
        }
        let expanded = expand_tilde(raw.trim());
        if !expanded.is_dir() {
            self.status = format!("pasta não encontrada: {}", expanded.display());
            return;
        }
        let pasta = expanded.canonicalize().unwrap_or(expanded);
        self.project_paths[idx] = pasta.clone();
        let nome = self.projects[idx].clone();
        if let Some(p) = self.config.projects.iter_mut().find(|p| p.name == nome) {
            p.path = pasta.clone();
        }
        let gravado = match &self.config_path {
            Some(cp) => self.config.save(cp).is_ok(),
            None => false,
        };
        if idx == self.project_idx {
            self.ensure_project_setup();
        }
        self.status = format!(
            "projeto \"{nome}\" agora em {}{}",
            pasta.display(),
            if gravado { "" } else { " (só nesta sessão — não consegui gravar o config)" }
        );
    }

    /// Terminal (o shell do dono) num card, na pasta da workspace.
    pub fn open_shell(&mut self) {
        let shell = std::env::var("SHELL").unwrap_or_else(|_| "bash".into());
        self.open_plain_card("terminal", &shell, &[]);
    }

    /// Sessão SSH interativa num card, num servidor cadastrado do projeto.
    pub fn open_ssh(&mut self, nome: &str) {
        let hosts = self.ssh_hosts();
        let Some(h) = hosts.iter().find(|h| h.nome.eq_ignore_ascii_case(nome.trim())) else {
            self.status = format!("servidor SSH \"{nome}\" não cadastrado neste projeto");
            return;
        };
        match orchestrator_core::ssh::write_config(self.project(), &hosts) {
            Ok(cfg) => {
                let args = vec!["-F".to_string(), cfg.display().to_string(), orchestrator_core::ssh::alias(&h.nome)];
                self.open_plain_card(&format!("ssh {}", h.nome), "ssh", &args);
            }
            Err(e) => self.status = format!("não consegui gravar a config SSH: {e}"),
        }
    }

    /// Card de terminal comum (não dirigido pelo orquestrador): quem digita
    /// é o dono.
    fn open_plain_card(&mut self, base: &str, program: &str, args: &[String]) {
        if self.ws().panes.len() >= MAX_PANES {
            self.status = format!("workspace {} cheia ({MAX_PANES} cards)", self.ws_idx + 1);
            return;
        }
        self.term_counter += 1;
        let name = format!("{base} #{}", self.term_counter);
        let cwd = self.workspace_dir();
        let env = self.cli_envs(&name, false);
        match TermSession::spawn_env(name.clone(), program, args, &cwd, 24, 80, &env) {
            Ok(session) => {
                let ws = self.ws();
                ws.panes.push(Pane::Term(Box::new(session)));
                ws.focused = ws.panes.len() - 1;
                self.events.push(EngineEvent::FocusGrid);
                self.status = format!("{name} aberto em {}", short_path(&cwd));
            }
            Err(err) => self.status = format!("erro abrindo {name}: {err}"),
        }
    }

    /// Servidores SSH cadastrados para o projeto ativo.
    pub fn ssh_hosts(&self) -> Vec<orchestrator_core::ssh::SshHost> {
        self.store
            .ui_get(&orchestrator_core::ssh::state_key(self.project()))
            .ok()
            .flatten()
            .map(|j| orchestrator_core::ssh::parse(&j))
            .unwrap_or_default()
    }

    pub fn set_ssh_hosts(&mut self, hosts: Vec<orchestrator_core::ssh::SshHost>) {
        let json = serde_json::to_string(&hosts).unwrap_or_else(|_| "[]".into());
        let _ = self.store.ui_set(&orchestrator_core::ssh::state_key(self.project()), &json);
        let _ = orchestrator_core::ssh::write_config(self.project(), &hosts);
        self.status = format!("{} servidor(es) SSH neste projeto", hosts.len());
    }

    /// Pastas fora do projeto que o dono liberou para as IAs (mesma chave que
    /// a trava lê em `crates/mcp-server/src/enforce.rs::allowed_dirs_key`).
    pub fn authorized_dirs(&self) -> Vec<String> {
        self.store
            .ui_get(&format!("pastas.autorizadas.{}", self.project()))
            .ok()
            .flatten()
            .map(|s| s.lines().filter(|l| !l.trim().is_empty()).map(str::to_string).collect())
            .unwrap_or_default()
    }

    /// `/liberar-pasta <caminho>` libera; `-<caminho>` revoga; sem nada, lista.
    pub fn authorize_dir(&mut self, raw: &str) {
        let raw = raw.trim();
        let mut atuais = self.authorized_dirs();
        if raw.is_empty() {
            self.status = if atuais.is_empty() {
                "nenhuma pasta fora do projeto liberada — as IAs pedem sua autorização antes".into()
            } else {
                format!("liberadas além do projeto: {}", atuais.join(", "))
            };
            return;
        }
        let (revogar, caminho) = match raw.strip_prefix('-') {
            Some(r) => (true, r.trim()),
            None => (false, raw),
        };
        let p = expand_tilde(caminho);
        let p = p.canonicalize().unwrap_or(p).display().to_string();
        atuais.retain(|a| a != &p);
        if !revogar {
            atuais.push(p.clone());
        }
        let _ = self.store.ui_set(&format!("pastas.autorizadas.{}", self.project()), &atuais.join("\n"));
        self.status = if revogar {
            format!("{p} voltou a precisar da sua autorização")
        } else {
            format!("as IAs deste projeto podem usar {p} sem pedir")
        };
    }

    /// Troca o projeto ativo (`/projeto [nome]`; sem nome, cicla) — leva a
    /// conversa persistida do projeto novo, e NENHUM card do projeto
    /// anterior fica visível ou comandável: as workspaces dele são
    /// arquivadas (os processos continuam rodando) e voltam intactas
    /// quando você trocar de volta para ele.
    pub fn switch_project(&mut self, name: Option<&str>) {
        let novo_idx = match name {
            Some(n) if !n.trim().is_empty() => {
                let n = n.trim();
                match self
                    .projects
                    .iter()
                    .position(|p| p.eq_ignore_ascii_case(n))
                {
                    Some(i) => i,
                    None => {
                        self.status = format!(
                            "projeto \"{n}\" não existe — conhecidos: {}",
                            self.projects.join(", ")
                        );
                        return;
                    }
                }
            }
            _ => (self.project_idx + 1) % self.projects.len(),
        };
        if novo_idx == self.project_idx {
            return;
        }
        self.persist_chat();
        // O chat ativo é um campo à parte (não mora em `workspaces[ws_idx]`
        // enquanto está em uso — só o das OUTRAS workspaces fica lá, ver
        // `switch_workspace`). Guarda-o na própria workspace antes de
        // arquivar o vetor inteiro, senão ele se perderia na troca.
        let provedor = self.chat.provider.clone();
        let ws_idx = self.ws_idx;
        self.workspaces[ws_idx].chat =
            Some(std::mem::replace(&mut self.chat, ChatState::new(provedor.clone())));
        let projeto_velho = self.projects[self.project_idx].clone();
        let arquivado = std::mem::take(&mut self.workspaces);
        self.workspaces_by_project.insert(projeto_velho, arquivado);

        self.project_idx = novo_idx;
        let projeto_novo = self.projects[novo_idx].clone();
        self.workspaces = self
            .workspaces_by_project
            .remove(&projeto_novo)
            .unwrap_or_else(|| (0..WS_COUNT).map(|_| Workspace::default()).collect());
        self.ws_idx = 0;

        self.chat = self.workspaces[0].chat.take().unwrap_or_else(|| ChatState::new(provedor));
        // Chat novo (primeira vez nesta workspace deste projeto): carrega a
        // conversa persistida — mesmo caminho de `switch_workspace`.
        if self.chat.transcript.is_empty() {
            self.restore_chat();
        }
        self.seen_decisions.clear();
        // Garante hook/MCP/regras do projeto novo IMEDIATAMENTE — antes só
        // acontecia por acaso, na primeira mensagem de chat: uma CLI aberta
        // (ou a sandbox pedida) logo depois de trocar, sem nunca ter
        // conversado, corria sem trava nenhuma no projeto que acabou de
        // entrar.
        self.ensure_project_setup();
        self.refresh_notices();
        self.reload();
        self.status = format!(
            "projeto ativo: {} ({})",
            self.project(),
            self.project_path().display()
        );
    }

    pub fn project_goal(&self) -> String {
        self.project_goals
            .get(self.project_idx)
            .cloned()
            .unwrap_or_default()
    }

    /// Troca a workspace ativa, levando o chat junto.
    ///
    /// Salva o que a atual produziu, guarda o chat dela no próprio workspace,
    /// e traz o da workspace destino (criando um novo se ela nunca teve).
    pub fn switch_workspace(&mut self, destino: usize) {
        let destino = destino.min(WS_COUNT - 1);
        if destino == self.ws_idx {
            return;
        }
        self.persist_chat();
        let atual = self.ws_idx;
        let provedor = self.chat.provider.clone();
        let guardado = self.workspaces[destino].chat.take();
        let anterior = std::mem::replace(
            &mut self.chat,
            guardado.unwrap_or_else(|| ChatState::new(provedor)),
        );
        self.workspaces[atual].chat = Some(anterior);
        self.ws_idx = destino;
        // Chat novo desta workspace: carrega a conversa dela do banco.
        if self.chat.transcript.is_empty() {
            self.restore_chat();
        }
        self.seen_decisions.clear();
        self.status = format!(
            "workspace {} · {}",
            destino + 1,
            short_path(&self.workspace_dir())
        );
    }

    pub fn ws(&mut self) -> &mut Workspace {
        &mut self.workspaces[self.ws_idx]
    }

    pub fn reload(&mut self) {
        self.pending = self.store.list_pending_decisions(true).unwrap_or_default();
        self.memories = self.store.list_visible(self.project(), None).unwrap_or_default();
        self.audit = self.store.list_decisions(self.project()).unwrap_or_default();
        self.tool_calls = self
            .store
            .recent_tool_calls(self.project(), 60)
            .unwrap_or_default();
        for p in &self.pending {
            if !self.projects.contains(&p.project) {
                self.projects.push(p.project.clone());
                self.project_paths
                    .push(std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
                self.project_goals.push(String::new());
            }
        }
        self.last_reload = Instant::now();
        // Canal preferencial: pendências novas também aparecem no chat.
        self.announce_new_decisions();
    }

    /// Anuncia no chat as decisões pendentes ainda não vistas — o chat é o
    /// canal PREFERENCIAL; o F2 continua listando tudo. Resolvíveis nos dois:
    /// `/aprovar`/`/negar` aqui, `a`/`d` lá.
    pub fn announce_new_decisions(&mut self) {
        // A chave inclui quem decide: uma pendência que o orquestrador passou
        // ao dono volta a ser anunciada, agora para o dono.
        let novas: Vec<PendingDecision> = self
            .pending
            .iter()
            .filter(|p| !self.seen_decisions.contains(&seen_key(p)))
            .cloned()
            .collect();
        if novas.is_empty() {
            return;
        }
        for p in novas {
            self.seen_decisions.insert(seen_key(&p));
            let id8 = &p.id[..8.min(p.id.len())];
            let resumo = p.summary.replace('\n', " ");
            if !p.is_for_owner() {
                // Modo autônomo: o pedido da CLI é do orquestrador. Ele recebe
                // como mensagem e decide; o dono só vê o que foi decidido.
                let quem = if p.requester.is_empty() {
                    "Uma CLI".to_string()
                } else {
                    format!("A CLI \"{}\"", p.requester)
                };
                self.chat.notify_system(format!(
                    "{quem} pediu confirmação [{id8}]: {resumo}. No modo autônomo quem decide é \
                     você: se bate com o que o dono pediu e é seguro, decision_resolve (id \
                     \"{id8}\", approve true, reason); se não bate, negue com o motivo. Se for \
                     MUITO crítico, fugir do pedido ou você não souber, decision_escalate. \
                     Depois avise a CLI com cli_send."
                ));
            } else if p.is_question() {
                let opcoes = if p.options.is_empty() {
                    "(resposta livre)".to_string()
                } else {
                    p.options
                        .iter()
                        .enumerate()
                        .map(|(i, o)| format!("{}) {o}", i + 1))
                        .collect::<Vec<_>>()
                        .join("   ")
                };
                let varias = if p.multiple { " — pode marcar várias" } else { "" };
                self.chat.push_line(
                    "decisão",
                    format!(
                        "❓ {resumo}\n{opcoes}{varias}\n/responder {id8} <número(s) ou texto>"
                    ),
                );
            } else {
                let porque = p
                    .note
                    .as_deref()
                    .map(|n| format!("\no orquestrador passou para você: {n}"))
                    .unwrap_or_default();
                self.chat.push_line(
                    "decisão",
                    format!(
                        "⚠ [{}] {} — pendente [{id8}]: {resumo}{porque}\n/aprovar {id8} · /negar {id8} (ou F2: a/d)",
                        p.project, p.created_at
                    ),
                );
            }
        }
        // Garante que o alerta fique visível (volta a acompanhar o fim).
        self.chat.scroll_from_end = 0;
    }

    /// Responde a uma pergunta ao dono e avisa o orquestrador (quando a
    /// pergunta é deste projeto) para ele seguir a partir da resposta.
    pub fn answer_question(&mut self, id: &str, resposta: &str) -> Result<(), String> {
        let q = self
            .store
            .answer_question(id, resposta)
            .map_err(|e| format!("não consegui responder: {e}"))?;
        let id8 = &q.id[..8.min(q.id.len())];
        self.chat
            .push_line("decisão", format!("✔ respondida [{id8}]: {resposta}"));
        if q.project == self.project() {
            self.chat.notify_system(format!(
                "O dono respondeu à pergunta \"{}\": {resposta}. Siga a partir daí.",
                q.summary.replace('\n', " ")
            ));
        }
        self.reload();
        Ok(())
    }

    /// `/responder <id> <resposta>`: número(s) das alternativas ou texto livre.
    pub fn answer_by_chat(&mut self, arg: &str) {
        let arg = arg.trim();
        let (prefixo, texto) = match arg.split_once(char::is_whitespace) {
            Some((p, t)) if !t.trim().is_empty() => (p, t.trim()),
            _ => {
                self.status = "uso: /responder <id> <número(s) da alternativa ou texto>".into();
                return;
            }
        };
        self.pending = self.store.list_pending_decisions(true).unwrap_or_default();
        let Some(q) = self
            .pending
            .iter()
            .find(|p| p.is_question() && p.id.starts_with(prefixo))
            .cloned()
        else {
            self.chat.push_line(
                "decisão",
                format!("nenhuma pergunta pendente com id começando em {prefixo}."),
            );
            return;
        };
        let resposta = interpret_answer(&q, texto);
        if let Err(msg) = self.answer_question(&q.id, &resposta) {
            self.chat.push_line("erro", msg);
        }
    }

    /// Resolve uma decisão pendente a partir do chat (`/aprovar [id]`).
    /// Sem id: a mais recente. Com id: casa por prefixo (id curto de 8 chars).
    pub fn resolve_by_chat(&mut self, approved: bool, id_prefix: Option<&str>) {
        // Fila fresca — a lista do reload pode estar defasada em até 2s.
        self.pending = self.store.list_pending_decisions(true).unwrap_or_default();
        // `list_pending_decisions` ordena created_at DESC: [0] é a mais nova.
        let target = match id_prefix {
            Some(prefix) => self
                .pending
                .iter()
                .find(|p| p.id.starts_with(prefix))
                .cloned(),
            // Sem id: a ação mais recente que espera o dono.
            None => self
                .pending
                .iter()
                .find(|p| p.is_for_owner() && !p.is_question())
                .cloned(),
        };
        let Some(p) = target else {
            self.chat.push_line(
                "decisão",
                match id_prefix {
                    Some(prefix) => {
                        format!("nenhuma decisão pendente com id começando em {prefix}.")
                    }
                    None => "nenhuma decisão pendente.".to_string(),
                },
            );
            return;
        };
        if p.is_question() {
            let id8 = &p.id[..8.min(p.id.len())];
            self.chat.push_line(
                "decisão",
                format!("[{id8}] é uma pergunta: responda com /responder {id8} <número(s) ou texto>."),
            );
            return;
        }
        let resolution = if approved {
            "aprovado pelo usuário no chat"
        } else {
            "negado pelo usuário no chat"
        };
        let id8 = p.id[..8.min(p.id.len())].to_string();
        match self.store.resolve_decision(&p.id, approved, resolution) {
            Ok(()) => {
                self.chat.push_line(
                    "decisão",
                    format!(
                        "{} [{id8}] {} — o agente retoma no próximo turno.",
                        if approved { "✔ aprovada" } else { "✖ negada" },
                        p.summary.replace('\n', " ")
                    ),
                );
                // Pedido de uma CLI: o orquestrador precisa saber para avisá-la.
                if !p.requester.is_empty() && p.requester != ORCHESTRATOR_AGENT_NAME && p.project == self.project() {
                    self.chat.notify_system(format!(
                        "O dono {} a decisão [{id8}] da CLI \"{}\": {}. Avise a CLI com cli_send{}.",
                        if approved { "aprovou" } else { "negou" },
                        p.requester,
                        p.summary.replace('\n', " "),
                        if approved { " para tentar de novo" } else { " e siga outro caminho" }
                    ));
                }
            }
            Err(err) => {
                self.chat
                    .push_line("erro", format!("erro resolvendo decisão {id8}: {err}"));
            }
        }
        self.reload();
    }

    /// Abre a CLI escolhida como novo terminal no workspace atual.
    pub fn open_cli(&mut self, spec_idx: usize) {
        let Some(spec) = self.clis.get(spec_idx).cloned() else {
            return;
        };
        if self.ws().panes.len() >= MAX_PANES {
            self.status = format!("workspace {} cheia ({MAX_PANES} cards)", self.ws_idx + 1);
            return;
        }
        self.term_counter += 1;
        let name = format!("{} #{}", spec.name, self.term_counter);
        let cwd = self.workspace_dir();
        let launch = match self.card_launch(&name, &spec.name, &[], false) {
            Ok(l) => l,
            Err(msg) => {
                self.status = msg;
                return;
            }
        };
        match TermSession::spawn_env(name.clone(), &launch.program, &launch.args, &cwd, 24, 80, &launch.env) {
            Ok(session) => {
                let ws = self.ws();
                ws.panes.push(Pane::Term(Box::new(session)));
                ws.focused = ws.panes.len() - 1;
                self.events.push(EngineEvent::FocusGrid);
                self.status = format!("{name} aberto — digite direto; Ctrl+q volta ao chat.");
            }
            Err(err) => {
                self.status = format!("erro abrindo {}: {err}", spec.name);
            }
        }
    }

    /// Garante hook PreToolUse + MCP + regras instalados no diretório do
    /// projeto (idempotente, write-if-changed) e avisa quando o gate não tem
    /// como agir: binários ausentes, ou NENHUMA regra `deny-regex:`/
    /// `ask-regex:` em memórias `security` — sem regra, o hook libera tudo e
    /// nenhuma decisão jamais aparece no chat/F2 (por desenho).
    pub fn ensure_project_setup(&mut self) {
        let project = self.project().to_string();
        match orchestrator_cli_adapter::claude_code::hooks::setup_project(
            &self.workspace_dir(),
            &project,
        ) {
            Ok(outcome) if !outcome.binaries_present => self.notices.push(
                "orchestrator-hook/orchestrator-mcp não estão ao lado do executável — \
                 o gate de segurança fica inativo (rode `cargo build` e abra a TUI pelo \
                 mesmo diretório)"
                    .into(),
            ),
            Ok(_) => {}
            Err(err) => self.notices.push(format!("setup do projeto falhou: {err}")),
        }
        // Gate sem regra nenhuma não levanta decisão POR DESENHO — em vez de
        // pedir cadastro manual, o projeto nasce com um conjunto sensato.
        match defaults::seed_default_security_rules(&self.store, &project) {
            Ok(n) if n > 0 => {
                self.status = format!(
                    "{n} regras de segurança padrão criadas para \"{project}\" (F2 → memórias para ver/editar)."
                );
                self.reload();
            }
            Ok(_) => {}
            Err(err) => self
                .notices
                .push(format!("não consegui semear as regras padrão: {err}")),
        }
    }

    /// Recalcula os avisos do estado atual. Eles NÃO vão para o rodapé: ficam
    /// na tela de manual (Ctrl+h), que é onde o usuário vai quando quer saber.
    pub fn refresh_notices(&mut self) {
        self.notices.clear();
        let project = self.project().to_string();
        match defaults::has_gate_rules(&self.store, &project) {
            Ok(false) => self.notices.push(format!(
                "o projeto \"{project}\" está sem regras de segurança: o gate libera tudo \
                 e nenhuma aprovação será pedida. Mande qualquer mensagem no chat para \
                 recriar o conjunto padrão, ou cadastre em F2 → memórias."
            )),
            Ok(true) => {}
            Err(err) => self.notices.push(format!("não consegui ler as regras: {err}")),
        }
        if !self.caps.supports("--disallowed-tools") {
            self.notices.push(
                "esta build do `claude` não tem --disallowed-tools: o orquestrador pode \
                 acabar editando arquivo em vez de delegar para uma CLI"
                    .into(),
            );
        }
        if !self.caps.supports("--mcp-config") {
            self.notices.push(
                "esta build do `claude` não tem --mcp-config: se o MCP do projeto não \
                 carregar sozinho, o orquestrador fica sem as tools cli_* e não conseguirá \
                 abrir CLIs"
                    .into(),
            );
        }
        let disponivel = providers::availability(&self.chat.provider, &Probe::current());
        if !disponivel.is_ready() {
            self.notices.push(format!(
                "o chat está com {}, que ainda não funciona: {} (/provedor troca)",
                self.chat.provider.name, disponivel.hint
            ));
        }
        if !self.notify_on_done {
            self.notices.push(
                "as notificações de \"CLI concluiu\" estão desligadas — o orquestrador não \
                 saberá quando uma CLI terminar (religue com /auto on)"
                    .into(),
            );
        }
    }

    /// Spawna um agente do orquestrador como card no workspace atual.
    ///
    /// `raw` é o texto após `/agente`, podendo conter flags inline no estilo
    /// "como uma pessoa faria": `model=opus effort=high mode=plan fast=on
    /// budget=2.5 <tarefa>`. Aliases aceitos: `modelo`/`m`, `esforco`/`e`,
    /// `modo`/`perm`/`p`, `rapido`, `orcamento`/`budget`. O que não for flag
    /// vira a tarefa. Defaults da config preenchem o que o usuário omitir.
    pub fn open_agent(&mut self, raw: String) {
        if self.ws().panes.len() >= MAX_PANES {
            self.status = format!("workspace {} cheia ({MAX_PANES} cards)", self.ws_idx + 1);
            return;
        }
        let parsed = self.parse_agent_flags(&raw);
        let mut options = parsed.options;
        if parsed.task.is_empty() {
            self.status =
                "uso: /agente [model=.. effort=.. mode=.. fast=on auto=on max=6] <tarefa>".into();
            return;
        }
        // Postura ativa (Shift+Tab) — só quando o usuário não deu `mode=`.
        if !parsed.explicit_mode {
            self.posture.apply(&mut options);
        }
        let mut autonomous = self.posture.autonomous() && !parsed.explicit_mode;
        // Garante hook PreToolUse + MCP + regras no projeto ANTES de spawnar
        // (idempotente) — sem isso o gate de segurança e a fila de decisões
        // não funcionam. Também avisa se o projeto não tem regra nenhuma.
        let project_dir = self.workspace_dir();
        self.ensure_project_setup();
        // Só emite flags que ESTA build da CLI suporta; avisa as ignoradas.
        let (_args, skipped) = options.to_args(&self.caps);
        if !skipped.is_empty() {
            // Zera as ignoradas p/ não passá-las adiante e deixa claro o motivo.
            self.strip_unsupported(&mut options, &skipped);
            // `--permission-prompt-tool` sumiu das builds novas do `claude`.
            // Sem ela, a postura pergunta-por-tool cairia no prompt nativo
            // headless e TRAVARIA sem canal de aprovação — então cai para o
            // gate autônomo do hook (que segue honrando ask-/deny-regex).
            if !parsed.explicit_mode
                && skipped.iter().any(|f| f == "--permission-prompt-tool")
            {
                autonomous = true;
            }
        }
        self.agent_counter += 1;
        let name = format!("Agente #{}", self.agent_counter);
        let label = agent_options_label(&options);
        let card = AgentCard::spawn(AgentSpec {
            name: name.clone(),
            task: parsed.task,
            project: self.project().to_string(),
            project_dir,
            options,
            db_path: Some(self.db_path.clone()),
            autonomous,
            auto: parsed.auto,
            max_iterations: parsed.max_iterations,
            goal: self.project_goal(),
        });
        let ws = self.ws();
        ws.panes.push(Pane::Agent(Box::new(card)));
        ws.focused = ws.panes.len() - 1;
        let mut tags = Vec::new();
        if !parsed.explicit_mode {
            tags.push(self.posture.label().to_string());
        }
        if !label.is_empty() {
            tags.push(label);
        }
        if parsed.auto {
            tags.push(format!("auto≤{}", parsed.max_iterations));
        }
        let tag = if tags.is_empty() {
            String::new()
        } else {
            format!(" [{}]", tags.join(" "))
        };
        let mut msg = format!(
            "{name} trabalhando no workspace {}{tag} — Tab foca p/ iterar · Ctrl+a autopilot · F2 decisões.",
            self.ws_idx + 1,
        );
        if !skipped.is_empty() {
            msg.push_str(&format!(" (esta CLI ignora: {})", skipped.join(", ")));
        }
        self.status = msg;
    }

    /// Interpreta flags inline e devolve opções + modo autônomo + tarefa.
    /// Defaults da config entram quando o usuário não especifica.
    pub fn parse_agent_flags(&self, raw: &str) -> ParsedAgent {
        let d = &self.agent_defaults;
        let mut model = non_empty(&d.model);
        let mut effort = non_empty(&d.effort);
        let mut mode = non_empty(&d.permission_mode);
        let mut fallback = non_empty(&d.fallback_model);
        let mut budget: Option<f64> = None;
        let mut fast = false;
        let mut auto = false;
        let mut explicit_mode = false;
        let mut max_iterations = DEFAULT_MAX_ITERATIONS;
        let mut task_words: Vec<&str> = Vec::new();

        let on = |v: &str| matches!(v, "on" | "1" | "true" | "sim" | "yes");
        for tok in raw.split_whitespace() {
            let Some((k, v)) = tok.split_once('=') else {
                task_words.push(tok);
                continue;
            };
            let v = v.trim();
            match k.to_ascii_lowercase().as_str() {
                "model" | "modelo" | "m" => model = non_empty(v),
                "effort" | "esforco" | "e" => effort = non_empty(v),
                "mode" | "modo" | "perm" | "p" => {
                    mode = non_empty(v);
                    explicit_mode = true;
                }
                "fallback" => fallback = non_empty(v),
                "budget" | "orcamento" | "max-budget-usd" => budget = v.parse().ok(),
                "fast" | "rapido" => fast = on(v),
                "auto" | "autopilot" | "autonomo" => auto = on(v),
                "max" | "iteracoes" | "iters" => {
                    if let Ok(n) = v.parse::<usize>() {
                        max_iterations = n.clamp(1, MAX_ITERATIONS_CAP);
                    }
                }
                // token com '=' que não é flag conhecida: parte da tarefa.
                _ => task_words.push(tok),
            }
        }
        // "modo rápido": Opus com saída acelerada — sem flag `-p` dedicada,
        // então aplicamos o modelo mais capaz como aproximação.
        if fast {
            model = Some("opus".into());
        }
        ParsedAgent {
            options: AgentOptions {
                model,
                effort,
                permission_mode: mode,
                fallback_model: fallback,
                max_budget_usd: budget,
                permission_prompt_tool: None,
            },
            auto,
            max_iterations,
            explicit_mode,
            task: task_words.join(" "),
        }
    }

    /// Avança cards em modo autônomo: continua a sessão rumo à meta, um passo
    /// por turno, pausando enquanto houver decisão pendente da sessão/projeto.
    /// Só toca o banco quando há de fato um card pronto para continuar.
    pub fn advance_autonomous(&mut self) {
        let any_candidate = self
            .workspaces
            .iter()
            .flat_map(|w| &w.panes)
            .any(|p| matches!(p, Pane::Agent(c) if c.wants_autocontinue()));
        if !any_candidate {
            return;
        }
        // Pendências frescas p/ a decisão de pausa (a lista de reload pode
        // estar atrasada logo após um `ask-regex` negar).
        self.pending = self.store.list_pending_decisions(true).unwrap_or_default();
        self.announce_new_decisions();
        let paused_sessions: std::collections::HashSet<String> =
            self.pending.iter().map(|p| p.session_id.clone()).collect();
        let paused_projects: std::collections::HashSet<String> =
            self.pending.iter().map(|p| p.project.clone()).collect();

        let mut note: Option<String> = None;
        for ws in &mut self.workspaces {
            for pane in &mut ws.panes {
                let Pane::Agent(card) = pane else { continue };
                if !card.wants_autocontinue() {
                    continue;
                }
                let paused = match &card.session_id {
                    Some(sid) => paused_sessions.contains(sid),
                    None => paused_projects.contains(&card.project),
                };
                if paused {
                    note = Some(format!(
                        "{} pausado — decisão pendente: /aprovar no chat ou F2 (a).",
                        card.name
                    ));
                    continue;
                }
                card.iterations += 1;
                let prompt = card.continuation_prompt();
                card.send_followup(prompt);
                note = Some(format!(
                    "{}: iteração autônoma {}/{} rumo à meta.",
                    card.name, card.iterations, card.max_iterations
                ));
            }
        }
        if let Some(n) = note {
            self.status = n;
        }
    }

    /// Remove das opções as flags que a CLI alvo não suporta.
    pub fn strip_unsupported(&self, o: &mut AgentOptions, skipped: &[String]) {
        for flag in skipped {
            match flag.as_str() {
                "--model" => o.model = None,
                "--effort" => o.effort = None,
                "--permission-mode" => o.permission_mode = None,
                "--fallback-model" => o.fallback_model = None,
                "--max-budget-usd" => o.max_budget_usd = None,
                "--permission-prompt-tool" => o.permission_prompt_tool = None,
                _ => {}
            }
        }
    }

    /// Monta uma linha legível do que a CLI `claude` suporta (`/caps`).
    pub fn caps_summary(&self) -> String {
        let c = &self.caps;
        if c.flags.is_empty() {
            return "`claude` não encontrado no PATH — nenhuma capacidade descoberta.".into();
        }
        let list = |v: &[String]| {
            if v.is_empty() {
                "(n/d)".to_string()
            } else {
                v.join(", ")
            }
        };
        format!(
            "CLI claude — modelos: {} | efforts: {} | permission-modes: {} | {} flags detectadas",
            list(&c.model_aliases),
            list(&c.efforts),
            list(&c.permission_modes),
            c.flags.len()
        )
    }

    /// Fecha o card focado do workspace atual.
    pub fn close_focused_pane(&mut self) {
        let ws = &mut self.workspaces[self.ws_idx];
        if ws.panes.is_empty() {
            return;
        }
        let idx = ws.focused.min(ws.panes.len() - 1);
        let pane = ws.panes.remove(idx);
        if let Pane::Term(mut t) = pane {
            t.kill();
        }
        if ws.focused >= ws.panes.len() && ws.focused > 0 {
            ws.focused -= 1;
        }
        if ws.panes.is_empty() {
            self.events.push(EngineEvent::FocusChat);
        }
    }

    /// Monta o preâmbulo que acompanha TODA mensagem do chat: onde ele está,
    /// quais CLIs comanda e o que já aconteceu.
    ///
    /// A memória NÃO entra aqui: o índice, as regras fixas do dono e as
    /// obrigações chegam pelo hook `UserPromptSubmit` a cada prompt — o
    /// mesmo texto que as CLIs recebem. Repetir aqui dobraria o custo em
    /// token e criaria duas versões da mesma instrução.
    pub fn build_context(&self, _message: &str) -> String {
        let project = self.project();
        let mut out = String::from("<orchestrator>\n");

        // 1. Onde ele está trabalhando.
        out.push_str(&format!(
            "WORKSPACE: projeto \"{project}\" · pasta {} · workspace {}/{WS_COUNT}\n",
            self.workspace_dir().display(),
            self.ws_idx + 1
        ));

        // 2. As CLIs que ele comanda agora (evita reabrir o que já existe).
        let clis: Vec<String> = self.workspaces[self.ws_idx]
            .panes
            .iter()
            .map(|p| match p {
                Pane::Term(t) if t.managed => format!("{} ({})", t.name, t.state().label()),
                Pane::Term(t) => format!("{} (terminal do usuário)", t.name),
                Pane::Agent(a) => format!("{} (agente headless)", a.name),
            })
            .collect();
        out.push_str(&format!(
            "CLIS ABERTAS: {}\n",
            if clis.is_empty() {
                "nenhuma — abra com cli_start quando for delegar".to_string()
            } else {
                clis.join(" · ")
            }
        ));

        // 3. O que já foi feito (auditoria recente = progresso real).
        if let Ok(audit) = self.store.list_decisions(project) {
            let recent: Vec<String> = audit
                .iter()
                .rev()
                .take(PROGRESS_LINES)
                .map(|d| {
                    let action = d.action.replace('\n', " ");
                    let action: String = action.chars().take(90).collect();
                    format!("- [{}] {action}", d.decision)
                })
                .collect();
            if !recent.is_empty() {
                out.push_str("PROGRESSO RECENTE (auditoria):\n");
                out.push_str(&recent.join("\n"));
                out.push('\n');
            }
        }

        out.push_str("</orchestrator>");
        out
    }

    /// Envia uma mensagem no chat pelo caminho único: monta o preâmbulo (que
    /// depende da mensagem), aplica os argumentos da build e dispara.
    ///
    /// `text` vindo `None` usa o que está digitado (caminho do Enter);
    /// `Some(..)` é mensagem pronta (fila do usuário ou notificação de CLI).
    pub fn send_chat(&mut self, text: Option<String>) {
        let message = match &text {
            Some(t) => t.clone(),
            None => self.chat.input.trim().to_string(),
        };
        if message.is_empty() {
            return;
        }
        // O chat roda `claude` com tools: garante hook/MCP/regras no projeto.
        if matches!(self.chat.provider.kind, ProviderKind::ClaudeCli) {
            self.ensure_project_setup();
            self.chat.extra_args = self.chat_extra_args();
        }
        self.chat.model_override = self.chat_model.clone();
        // Variáveis da ferramenta (endpoint e chave de outro fornecedor).
        match self.chat.provider.resolved_env(&|k| std::env::var(k).ok()) {
            Ok(env) => self.chat.env = env,
            Err(var) => {
                self.chat.push_line(
                    "erro",
                    format!(
                        "{} precisa da variável {var}: defina e reabra o Orchestrator.",
                        self.chat.provider.name
                    ),
                );
                return;
            }
        }
        let context = self.build_context(&message);
        let project = self.project().to_string();
        let dir = self.workspace_dir();
        let db = Some(self.db_path.clone());
        match text {
            Some(t) => self.chat.send_text(context, &project, dir, db, t),
            None => self.chat.send(context, &project, dir, db),
        }
    }

    /// Modelos oferecidos no seletor do provedor ativo.
    ///
    /// Para o Claude CLI vêm das capacidades da build (lidas do `--help`);
    /// para provedores HTTP, o modelo configurado. A primeira opção é sempre
    /// "padrão", que devolve a escolha ao provedor.
    pub fn model_options(&self) -> Vec<String> {
        let mut out = vec![DEFAULT_MODEL_LABEL.to_string()];
        let p = &self.chat.provider;
        match p.kind {
            ProviderKind::ClaudeCli if p.env.is_empty() => {
                out.extend(self.caps.model_aliases.iter().cloned())
            }
            // Claude Code noutro endpoint: os modelos daquele fornecedor.
            ProviderKind::ClaudeCli => {
                for k in [
                    "ANTHROPIC_DEFAULT_OPUS_MODEL",
                    "ANTHROPIC_DEFAULT_SONNET_MODEL",
                    "ANTHROPIC_DEFAULT_HAIKU_MODEL",
                ] {
                    if let Some(m) = p.env.get(k) {
                        if !out.contains(m) {
                            out.push(m.clone());
                        }
                    }
                }
            }
            _ => {
                if let Some(vivos) = self.live_models.get(&p.name) {
                    // Veio da API do provedor: a lista de verdade, ordenada.
                    let mut vivos = vivos.clone();
                    vivos.sort();
                    out.extend(vivos);
                } else if !p.model.is_empty() {
                    out.push(p.model.clone());
                }
            }
        }
        out.dedup();
        out
    }

    /// A "tela virtual" ao vivo de uma sandbox aberta pelo orquestrador
    /// (`ui_open`/`ui_exec` no `orchestrator-mcp`): caminho do PNG mais
    /// recente e quando foi tirado. `None` sem sandbox aberta com esse nome
    /// — a thread de captura escreve isto no `ui_state`, e some de lá
    /// quando a sandbox é encerrada (`ui_stop`).
    ///
    /// A sandbox roda no PROCESSO do `orchestrator-mcp`, separado do app e
    /// da TUI — por isso a ponte é o banco compartilhado, não uma chamada
    /// direta.
    pub fn sandbox_live(&self, name: &str) -> Option<(PathBuf, String)> {
        // Mesmo prefixo de projeto que `iniciar_tela_viva` grava
        // (`crates/mcp-server/src/ui.rs`) — sem ele leríamos (ou, pior,
        // misturaríamos com) a tela de uma sandbox de OUTRO projeto com o
        // mesmo nome.
        let chave = format!("{}.{}", sandbox_sanitize(self.project()), sandbox_sanitize(name));
        let caminho = self
            .store
            .ui_get(&format!("sandbox.{chave}.tela_viva"))
            .ok()??;
        let em = self
            .store
            .ui_get(&format!("sandbox.{chave}.tela_viva_em"))
            .ok()
            .flatten()
            .unwrap_or_default();
        Some((PathBuf::from(caminho), em))
    }

    /// A lista de modelos do provedor ativo está sendo buscada agora?
    pub fn models_loading(&self) -> bool {
        self.models_loading.as_deref() == Some(self.chat.provider.name.as_str())
    }

    /// Este provedor tem uma API de listagem de modelos (`GET /models`)?
    fn models_api_url(&self) -> Option<(String, Option<String>)> {
        let p = &self.chat.provider;
        (matches!(p.kind, ProviderKind::OpenAiCompat) && !p.base_url.is_empty()).then(|| {
            let key = (!p.api_key_env.is_empty())
                .then(|| std::env::var(&p.api_key_env).ok())
                .flatten();
            (p.base_url.clone(), key)
        })
    }

    /// Pede à API do provedor ativo a lista de modelos dela, em segundo
    /// plano — chame ao abrir o seletor. Sem efeito se o provedor não tiver
    /// essa API, ou se já tiver uma busca dele em andamento.
    pub fn refresh_models(&mut self) {
        let Some((base_url, api_key)) = self.models_api_url() else {
            return;
        };
        let provider = self.chat.provider.name.clone();
        if self.models_loading.as_deref() == Some(provider.as_str()) {
            return; // já buscando este provedor
        }
        self.models_loading = Some(provider.clone());
        let tx = self.models_tx.clone();
        std::thread::spawn(move || {
            let rt = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
                Ok(rt) => rt,
                Err(e) => {
                    let _ = tx.send((provider, Err(format!("runtime tokio: {e}"))));
                    return;
                }
            };
            let resultado = rt
                .block_on(orchestrator_llm::list_models(&base_url, api_key.as_deref()))
                .map_err(|e| format!("{e:#}"));
            let _ = tx.send((provider, resultado));
        });
    }

    /// Manda "." ao modelo escolhido pela API do provedor ativo (ou, sem uma
    /// API de listagem, pelo modelo configurado) e devolve se ele processa e
    /// responde — em segundo plano, sem travar a interface. O resultado sai
    /// em [`Engine::last_model_test`], drenado por [`Engine::drain_background`].
    pub fn test_model(&mut self, model: &str) {
        let provider = self.chat.provider.clone();
        let model = if model.is_empty() || model == DEFAULT_MODEL_LABEL {
            provider.model.clone()
        } else {
            model.to_string()
        };
        let chave = format!("{}/{model}", provider.name);
        if !matches!(provider.kind, ProviderKind::OpenAiCompat) {
            self.last_model_test = Some((
                chave,
                Err(format!(
                    "testar modelo direto só existe para provedores HTTP; para {} converse \
                     mesmo no chat",
                    provider.kind.tool_label()
                )),
            ));
            return;
        }
        if provider.base_url.is_empty() {
            self.last_model_test = Some((chave, Err("provedor sem endereço configurado".into())));
            return;
        }
        let api_key = (!provider.api_key_env.is_empty())
            .then(|| std::env::var(&provider.api_key_env).ok())
            .flatten();
        if !provider.api_key_env.is_empty() && api_key.is_none() {
            self.last_model_test =
                Some((chave, Err(format!("falta a variável {}", provider.api_key_env))));
            return;
        }
        self.last_model_test = Some((chave.clone(), Err("testando…".into())));
        let tx = self.model_test_tx.clone();
        std::thread::spawn(move || {
            let rt = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
                Ok(rt) => rt,
                Err(e) => {
                    let _ = tx.send((chave, Err(format!("runtime tokio: {e}"))));
                    return;
                }
            };
            let resultado = rt
                .block_on(orchestrator_llm::probe_model(
                    &provider.base_url,
                    api_key.as_deref(),
                    &model,
                ))
                .map(|(texto, duracao)| {
                    let resumo: String = texto.split_whitespace().collect::<Vec<_>>().join(" ");
                    let resumo: String = resumo.chars().take(80).collect();
                    format!("respondeu em {}ms: {resumo}", duracao.as_millis())
                })
                .map_err(|e| format!("{e:#}"));
            let _ = tx.send((chave, resultado));
        });
    }

    /// Drena os fetches de modelo e testes em andamento. Chame a cada tick.
    fn drain_models(&mut self) {
        while let Ok((provider, resultado)) = self.models_rx.try_recv() {
            if self.models_loading.as_deref() == Some(provider.as_str()) {
                self.models_loading = None;
            }
            match resultado {
                Ok(ids) if !ids.is_empty() => {
                    self.live_models.insert(provider, ids);
                }
                Ok(_) => {
                    self.status = format!("{provider}: a API não listou nenhum modelo");
                }
                Err(e) => {
                    self.status = format!("{provider}: não consegui listar os modelos ({e})");
                }
            }
        }
        while let Ok(resultado) = self.model_test_rx.try_recv() {
            self.last_model_test = Some(resultado);
        }
    }

    /// Os provedores e o estado de cada um agora (a lista do `/provedor`).
    pub fn provider_list(&self) -> Vec<(LlmProvider, Availability)> {
        let probe = Probe::current();
        self.providers
            .iter()
            .map(|p| (p.clone(), providers::availability(p, &probe)))
            .collect()
    }

    /// Posição do provedor ativo na lista.
    pub fn provider_index(&self) -> usize {
        self.providers
            .iter()
            .position(|p| p.name == self.chat.provider.name)
            .unwrap_or(0)
    }

    /// Escolha do usuário na lista.
    ///
    /// Pronto: passa a responder. Falta entrar na conta: troca e abre um card
    /// com o login da CLI, para a pessoa entrar ali mesmo. Falta instalar ou
    /// falta a chave: não troca, e diz o que fazer.
    pub fn choose_provider(&mut self, idx: usize) {
        let Some(p) = self.providers.get(idx).cloned() else {
            return;
        };
        if self.chat.busy && p.name != self.chat.provider.name {
            self.status = format!(
                "espere o turno de {} terminar para trocar de provedor",
                self.chat.provider.name
            );
            return;
        }
        let a = providers::availability(&p, &Probe::current());
        match a.state {
            ProviderState::Ready => {
                self.set_chat_provider(idx);
                self.status = format!("chat com {} ({})", p.name, p.kind.tool_label());
            }
            ProviderState::NeedsLogin => {
                self.set_chat_provider(idx);
                let Some(cmd) = a.fix_command.clone() else {
                    self.status = format!("{}: {}", p.name, a.hint);
                    return;
                };
                let mut partes = cmd.split_whitespace().map(str::to_string);
                let binario = partes.next().unwrap_or_default();
                let args: Vec<String> = partes.collect();
                let nome = format!("login-{binario}");
                match self.start_named_cli(&nome, &binario, &args, None) {
                    Ok(_) => {
                        self.events.push(EngineEvent::FocusGrid);
                        self.status = format!(
                            "{} precisa da sua conta: entre no card \"{nome}\" e depois é só \
                             mandar a mensagem.",
                            p.name
                        );
                    }
                    Err(msg) => self.status = format!("{}: {} ({msg})", p.name, a.hint),
                }
            }
            ProviderState::NotInstalled | ProviderState::MissingKey => {
                self.status = format!("{}: {}", p.name, a.hint);
            }
        }
    }

    /// `/provedor <nome>`: o nome inteiro (sem diferenciar maiúsculas), ou um
    /// pedaço que só um provedor tenha ("codex", "antigravity").
    pub fn choose_provider_by_name(&mut self, query: &str) {
        let q = query.trim().to_lowercase();
        let nomes: Vec<String> = self.providers.iter().map(|p| p.name.to_lowercase()).collect();
        let contem: Vec<usize> = (0..nomes.len()).filter(|&i| nomes[i].contains(&q)).collect();
        let comeca: Vec<usize> = contem
            .iter()
            .copied()
            .filter(|&i| nomes[i].starts_with(&q))
            .collect();
        let escolhido = nomes
            .iter()
            .position(|n| *n == q)
            .or_else(|| (contem.len() == 1).then(|| contem[0]))
            .or_else(|| (comeca.len() == 1).then(|| comeca[0]));
        match escolhido {
            Some(i) => self.choose_provider(i),
            None if contem.is_empty() => {
                self.status = format!("nenhum provedor com \"{}\" — /provedor abre a lista", query.trim())
            }
            None => {
                let opcoes: Vec<&str> =
                    contem.iter().map(|&i| self.providers[i].name.as_str()).collect();
                self.status = format!(
                    "\"{}\" serve para mais de um: {} — diga qual",
                    query.trim(),
                    opcoes.join(", ")
                );
            }
        }
    }

    /// Define o modelo do chat. `""` (ou "padrão") volta ao do provedor.
    /// Aceita nome livre: builds novas podem ter modelos que o help não lista.
    pub fn set_chat_model(&mut self, model: &str) {
        let model = model.trim();
        let model = if model.is_empty() || model == DEFAULT_MODEL_LABEL {
            String::new()
        } else {
            model.to_string()
        };
        let key = format!("chat.model.{}", self.chat.provider.name);
        if model.is_empty() {
            let _ = self.store.ui_delete(&key);
        } else {
            let _ = self.store.ui_set(&key, &model);
        }
        // Provedor HTTP usa o campo do provedor; a CLI usa `--model`.
        if matches!(self.chat.provider.kind, ProviderKind::OpenAiCompat) && !model.is_empty() {
            self.chat.provider.model = model.clone();
        }
        self.chat_model = model;
        self.status = format!(
            "chat: {} · modelo {}",
            self.chat.provider.name,
            if self.chat_model.is_empty() {
                "padrão do provedor".to_string()
            } else {
                self.chat_model.clone()
            }
        );
    }

    /// Troca o provedor do chat, sem conferir se ele funciona (quem confere é
    /// [`Engine::choose_provider`]): guarda a conversa, retoma a sessão que
    /// este provedor já tinha nesta workspace e o modelo salvo para ele.
    pub fn set_chat_provider(&mut self, idx: usize) {
        let Some(p) = self.providers.get(idx).cloned() else {
            return;
        };
        let name = p.name.clone();
        if name != self.chat.provider.name {
            self.persist_chat();
            let project = self.project().to_string();
            let key = provider_session_key(&project, self.ws_idx, &name);
            let session = self.store.ui_get(&key).ok().flatten();
            self.saved_session = session.clone();
            self.chat.switch_provider(p, session);
            let _ = self.store.ui_set(CHAT_PROVIDER_KEY, &name);
        } else {
            self.chat.provider = p;
        }
        let saved = self
            .store
            .ui_get(&format!("chat.model.{name}"))
            .ok()
            .flatten()
            .unwrap_or_default();
        self.set_chat_model(&saved);
    }

    /// O que a paleta precisa saber para dizer o estado de cada comando.
    pub fn palette_context(&self) -> palette::Context {
        palette::Context {
            // Só o que espera o DONO: o que o orquestrador está decidindo não
            // pede nada de quem está na tela.
            pending_decisions: self
                .pending
                .iter()
                .filter(|p| p.is_for_owner() && !p.is_question())
                .count(),
            open_questions: self.pending.iter().filter(|p| p.is_question()).count(),
            open_clis: self.workspaces[self.ws_idx].panes.len(),
            max_clis: MAX_PANES,
            projects: self.projects.len(),
            has_claude: !self.caps.flags.is_empty(),
            notify_on_done: self.notify_on_done,
            memories: self.memories.len(),
            has_dir_picker: picker_dir::available(),
        }
    }

    /// Comandos que casam o que está digitado agora (vazio = paleta fechada).
    pub fn palette_entries(&self) -> Vec<palette::Entry> {
        if !palette::should_open(&self.chat.input) {
            return Vec::new();
        }
        palette::filter(&self.chat.input, &self.palette_context())
    }

    /// Argumentos extras do `claude` do CHAT.
    ///
    /// Duas coisas essenciais, ambas condicionadas às capacidades da build:
    /// 1. `--disallowed-tools`: o orquestrador NÃO escreve arquivo nem abre
    ///    subagente — ele delega para CLIs (pedido explícito do usuário).
    /// 2. `--mcp-config <projeto>/.mcp.json`: em modo headless o `.mcp.json`
    ///    do projeto pode não ser carregado sozinho, e sem ele o orquestrador
    ///    não teria as tools `cli_*` — ou seja, não conseguiria abrir CLI
    ///    nenhuma. Passar o arquivo explicitamente elimina esse risco.
    pub fn chat_extra_args(&self) -> Vec<String> {
        let mut args = Vec::new();
        if self.caps.supports("--disallowed-tools") {
            args.push("--disallowed-tools".to_string());
            args.push("Write Edit MultiEdit NotebookEdit Task".to_string());
        }
        if !self.chat_model.is_empty() && self.caps.supports("--model") {
            args.push("--model".to_string());
            args.push(self.chat_model.clone());
        }
        // A sandbox monta esta pasta por padrão (ui_open sem workdir).
        std::env::set_var("ORCHESTRATOR_WORKDIR", self.workspace_dir());
        // O chat é o orquestrador: é o nome dele nas memórias que gravar. As
        // CLIs recebem o próprio nome explicitamente em `cli_envs`.
        std::env::set_var("ORCHESTRATOR_AGENT", "orquestrador");
        if self.caps.supports("--mcp-config") {
            let mcp = self.workspace_dir().join(".mcp.json");
            if mcp.is_file() {
                args.push("--mcp-config".to_string());
                args.push(mcp.display().to_string());
            }
        }
        args
    }

    /// Ambiente exportado às CLIs abertas pela TUI: o hook `PreToolUse`
    /// instalado no projeto precisa saber QUAL banco e projeto usar.
    ///
    /// `managed` (CLI aberta pelo ORQUESTRADOR) também recebe
    /// `ORCHESTRATOR_AUTONOMOUS=1`: ninguém está digitando nela, então uma
    /// confirmação na tela travaria o trabalho para sempre. Com isso o hook
    /// vira a autoridade — libera tool limpa explicitamente e o que casar
    /// `ask-regex`/`deny-regex` continua virando decisão/bloqueio. Nunca é
    /// `bypassPermissions`. Numa CLI manual (Ctrl+t) o usuário está lá para
    /// responder, então ela não recebe a variável.
    pub fn cli_envs(&self, agent: &str, managed: bool) -> Vec<(String, String)> {
        let mut envs = vec![
            (
                "ORCHESTRATOR_DB".to_string(),
                self.db_path.display().to_string(),
            ),
            ("ORCHESTRATOR_PROJECT".to_string(), self.project().to_string()),
            // Autor das memórias que esta IA gravar (e quem a instrução
            // invisível chama pelo nome).
            ("ORCHESTRATOR_AGENT".to_string(), agent.to_string()),
            // A pasta que a sandbox desta CLI monta por padrão (`ui_open`
            // sem `workdir`). Antes só o CHAT do orquestrador publicava isto
            // (e só para SI MESMO, via uma variável de processo global) —
            // as CLIs abertas em cards não tinham isto no próprio ambiente
            // nenhum, então `ui_open` sem workdir caía vazio ou pegava a
            // pasta de OUTRO projeto que por acaso tivesse passado por ali.
            (
                "ORCHESTRATOR_WORKDIR".to_string(),
                self.workspace_dir().display().to_string(),
            ),
        ];
        if managed {
            envs.push(("ORCHESTRATOR_AUTONOMOUS".to_string(), "1".to_string()));
        }
        envs
    }

    /// Como abrir uma CLI num card.
    ///
    /// `command` é o binário (`codex`) ou o nome de uma CLI cadastrada
    /// ("Claude Code · GLM"), que traz o comando, os argumentos e as variáveis
    /// dela. O hook fica sabendo qual ferramenta é (`ORCHESTRATOR_HARNESS`).
    /// Codex, Kimi e OpenCode recebem o servidor MCP do Orchestrator em modo
    /// trava, cada um no seu formato; o Claude Code já o acha no `.mcp.json` do
    /// projeto, e o Antigravity só lê MCP da config global.
    pub fn card_launch(
        &self,
        agent: &str,
        command: &str,
        args: &[String],
        managed: bool,
    ) -> Result<CardLaunch, String> {
        let cadastrada = self
            .clis
            .iter()
            .find(|c| c.name.eq_ignore_ascii_case(command.trim()));
        let (program, mut full_args) = match cadastrada {
            Some(c) => (c.command.clone(), c.args.iter().chain(args).cloned().collect::<Vec<_>>()),
            None => (command.to_string(), args.to_vec()),
        };
        let base_env = self.cli_envs(agent, managed);
        let mut env = base_env.clone();
        if let Some(c) = cadastrada {
            for (k, v) in &c.env {
                let valor = orchestrator_core::config::expand_vars(v, &|k| std::env::var(k).ok())
                    .map_err(|var| format!("a CLI \"{}\" precisa da variável {var}", c.name))?;
                env.push((k.clone(), valor));
            }
        }
        let binario = std::path::Path::new(&program)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or(&program)
            .to_string();
        let ferramenta = match binario.as_str() {
            "claude" => Some("claude"),
            "codex" => Some("codex"),
            "kimi" => Some("kimi"),
            "agy" => Some("antigravity"),
            "opencode" => Some("opencode"),
            _ => None,
        };
        if let Some(f) = ferramenta {
            env.push(("ORCHESTRATOR_HARNESS".into(), f.into()));
        }
        let mcp = matches!(binario.as_str(), "codex" | "kimi" | "opencode")
            .then(|| harness::sibling_binary("orchestrator-mcp"))
            .flatten();
        if let Some(mcp) = mcp {
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0);
            let mut mcp_env = base_env;
            mcp_env.push(("ORCHESTRATOR_GATE_IN_MCP".into(), "1".into()));
            mcp_env.push(("ORCHESTRATOR_SESSION".into(), format!("card-{agent}-{nanos:x}")));
            let mcp_path = mcp.display().to_string();
            match binario.as_str() {
                "codex" => {
                    let extra = [
                        "-c".to_string(),
                        format!("mcp_servers.orchestrator.command={}", harness::toml_str(&mcp_path)),
                        "-c".to_string(),
                        format!("mcp_servers.orchestrator.env={}", harness::toml_inline_table(&mcp_env)),
                    ];
                    full_args.splice(0..0, extra);
                }
                "kimi" => {
                    let config = serde_json::json!({ "mcpServers": { "orchestrator": {
                        "command": mcp_path, "args": [], "env": harness::json_env(&mcp_env)
                    }}});
                    full_args.splice(0..0, ["--mcp-config".to_string(), config.to_string()]);
                }
                _ => {
                    let config = serde_json::json!({ "mcp": { "orchestrator": {
                        "type": "local", "command": [mcp_path],
                        "environment": harness::json_env(&mcp_env), "enabled": true
                    }}});
                    env.push(("OPENCODE_CONFIG_CONTENT".into(), config.to_string()));
                }
            }
        }
        Ok(CardLaunch {
            program,
            args: full_args,
            env,
        })
    }

    /// Guarda quais CLIs esta workspace tinha abertas, para poder reabrir.
    ///
    /// O processo não sobrevive ao fechamento da TUI, mas a COMPOSIÇÃO da
    /// workspace sim: nomes e comandos voltam como sugestão em vez de o
    /// usuário ter que lembrar o que tinha montado.
    pub fn save_workspace_clis(&self) {
        let clis: Vec<serde_json::Value> = self.workspaces[self.ws_idx]
            .panes
            .iter()
            .filter_map(|p| match p {
                Pane::Term(t) if t.managed => Some(serde_json::json!({
                    "name": t.name,
                    "last_prompt": t.last_prompt,
                })),
                _ => None,
            })
            .collect();
        let key = format!("ws.{}.{}.clis", self.project(), self.ws_idx);
        if clis.is_empty() {
            let _ = self.store.ui_delete(&key);
        } else {
            let _ = self
                .store
                .ui_set(&key, &serde_json::Value::Array(clis).to_string());
        }
    }

    /// Nomes das CLIs que esta workspace tinha na última sessão.
    pub fn saved_workspace_clis(&self) -> Vec<String> {
        let key = format!("ws.{}.{}.clis", self.project(), self.ws_idx);
        self.store
            .ui_get(&key)
            .ok()
            .flatten()
            .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
            .and_then(|v| v.as_array().cloned())
            .map(|a| {
                a.iter()
                    .filter_map(|c| c.get("name").and_then(|n| n.as_str()).map(str::to_string))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Acha uma CLI gerenciada por nome em qualquer workspace.
    pub fn find_cli(&mut self, name: &str) -> Option<(usize, usize)> {
        for (wi, ws) in self.workspaces.iter().enumerate() {
            for (pi, pane) in ws.panes.iter().enumerate() {
                if let Pane::Term(t) = pane {
                    if t.name == name {
                        return Some((wi, pi));
                    }
                }
            }
        }
        None
    }

    /// Abre uma CLI REAL nomeada como card (caminho do orquestrador e do
    /// comando `/cli`). Devolve a mensagem de resultado (ok ou erro).
    pub fn start_named_cli(
        &mut self,
        name: &str,
        command: &str,
        args: &[String],
        cwd: Option<PathBuf>,
    ) -> Result<String, String> {
        let name = name.trim();
        if name.is_empty() {
            return Err("o nome da CLI não pode ser vazio".into());
        }
        if self.find_cli(name).is_some() {
            return Err(format!(
                "já existe uma CLI chamada \"{name}\" — use cli_send para \
                 mandar tarefa nela, ou cli_stop para encerrá-la"
            ));
        }
        if self.ws().panes.len() >= MAX_PANES {
            return Err(format!(
                "workspace {} cheia ({MAX_PANES} cards) — feche um card ou \
                 troque de workspace (Alt+↑/↓)",
                self.ws_idx + 1
            ));
        }
        let dir = cwd.unwrap_or_else(|| self.workspace_dir());
        let mut launch = self.card_launch(name, command, args, true)?;
        // `cli_envs` (dentro de `card_launch`) supõe a pasta da workspace
        // ativa — corrige aqui quando esta CLI nasceu com um `cwd` explícito
        // diferente (payload de `cli_start`), senão `ui_open` sem `workdir`
        // montaria a pasta errada.
        if dir != self.workspace_dir() {
            let valor = dir.display().to_string();
            match launch.env.iter_mut().find(|(k, _)| k == "ORCHESTRATOR_WORKDIR") {
                Some((_, v)) => *v = valor,
                None => launch.env.push(("ORCHESTRATOR_WORKDIR".to_string(), valor)),
            }
        }
        match TermSession::spawn_env(name.to_string(), &launch.program, &launch.args, &dir, 24, 80, &launch.env) {
            Ok(mut session) => {
                session.managed = true;
                let ws_no = self.ws_idx + 1;
                let ws = self.ws();
                ws.panes.push(Pane::Term(Box::new(session)));
                ws.focused = ws.panes.len() - 1;
                let _ = self.store.upsert_cli_state(
                    &self.projects[self.project_idx].clone(),
                    name,
                    CliState::Starting.label(),
                    "",
                );
                Ok(format!(
                    "CLI \"{name}\" aberta na workspace {ws_no} ({command}) em {}",
                    dir.display()
                ))
            }
            Err(err) => Err(format!("não consegui abrir `{command}`: {err}")),
        }
    }

    /// Consome a fila `cli_commands` que o orquestrador (via MCP) enfileirou.
    ///
    /// A TUI é o único processo que pode mexer nos PTYs; o MCP roda como
    /// filho do `claude` e conversa com ela por esta fila no SQLite.
    pub fn poll_cli_commands(&mut self) {
        let project = self.project().to_string();
        let cmds = match self.store.pending_cli_commands(&project) {
            Ok(c) => c,
            Err(_) => return,
        };
        for cmd in cmds {
            let outcome = match cmd.kind.as_str() {
                "start" => {
                    let payload: serde_json::Value =
                        serde_json::from_str(&cmd.payload).unwrap_or_default();
                    let command = payload
                        .get("command")
                        .and_then(|v| v.as_str())
                        .filter(|c| !c.trim().is_empty())
                        .unwrap_or("claude")
                        .to_string();
                    let args: Vec<String> = payload
                        .get("args")
                        .and_then(|v| v.as_array())
                        .map(|a| {
                            a.iter()
                                .filter_map(|v| v.as_str().map(str::to_string))
                                .collect()
                        })
                        .unwrap_or_default();
                    let cwd = payload
                        .get("cwd")
                        .and_then(|v| v.as_str())
                        .filter(|c| !c.trim().is_empty())
                        .map(PathBuf::from);
                    // Instala hook/MCP/regras na pasta onde a CLI vai rodar.
                    self.ensure_project_setup();
                    self.start_named_cli(&cmd.cli_name, &command, &args, cwd)
                }
                "send" => match self.find_cli(&cmd.cli_name) {
                    Some((wi, pi)) => {
                        if let Some(Pane::Term(t)) = self.workspaces[wi].panes.get_mut(pi) {
                            if t.is_exited() {
                                Err(format!(
                                    "a CLI \"{}\" já encerrou — abra outra com cli_start",
                                    cmd.cli_name
                                ))
                            } else {
                                t.managed = true;
                                match t.send_prompt(&cmd.payload) {
                                    PromptDelivery::Sent => Ok(format!(
                                        "prompt entregue à CLI \"{}\" — você será \
                                         notificado quando ela concluir",
                                        cmd.cli_name
                                    )),
                                    PromptDelivery::Deferred => Ok(format!(
                                        "prompt guardado: a CLI \"{}\" ainda está \
                                         subindo; ele é digitado sozinho quando ela \
                                         ficar pronta, e você será notificado ao fim",
                                        cmd.cli_name
                                    )),
                                }
                            }
                        } else {
                            Err(format!("CLI \"{}\" não encontrada", cmd.cli_name))
                        }
                    }
                    None => Err(format!(
                        "nenhuma CLI chamada \"{}\" — abra com cli_start antes de \
                         mandar tarefa",
                        cmd.cli_name
                    )),
                },
                "read" => match self.find_cli(&cmd.cli_name) {
                    Some((wi, pi)) => {
                        let payload: serde_json::Value =
                            serde_json::from_str(&cmd.payload).unwrap_or_default();
                        let search = payload
                            .get("search")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .trim()
                            .to_string();
                        let lines = payload
                            .get("lines")
                            .and_then(|v| v.as_u64())
                            .unwrap_or(40)
                            .clamp(1, 400) as usize;
                        let context = payload
                            .get("context")
                            .and_then(|v| v.as_u64())
                            .unwrap_or(3)
                            .clamp(0, 20) as usize;
                        match self.workspaces[wi].panes.get_mut(pi) {
                            Some(Pane::Term(t)) => {
                                let out = if search.is_empty() {
                                    t.history(lines)
                                } else {
                                    t.search(&search, context, SEARCH_MAX_HITS)
                                };
                                if out.is_empty() {
                                    Ok(if search.is_empty() {
                                        format!("a CLI \"{}\" ainda não escreveu nada", cmd.cli_name)
                                    } else {
                                        format!(
                                            "nada com \"{search}\" no histórico de \"{}\"",
                                            cmd.cli_name
                                        )
                                    })
                                } else {
                                    Ok(out.join("\n"))
                                }
                            }
                            _ => Err(format!("CLI \"{}\" não encontrada", cmd.cli_name)),
                        }
                    }
                    None => Err(format!(
                        "nenhuma CLI chamada \"{}\" — cli_status lista as abertas",
                        cmd.cli_name
                    )),
                },
                "menu" => match self.find_cli(&cmd.cli_name) {
                    Some((wi, pi)) => match self.workspaces[wi].panes.get(pi) {
                        Some(Pane::Term(t)) => match t.menu() {
                            Some(menu) => {
                                let mut out = String::new();
                                if !menu.question.is_empty() {
                                    out.push_str(&format!("{}\n", menu.question));
                                }
                                for (i, o) in menu.options.iter().enumerate() {
                                    out.push_str(&format!(
                                        "{} {}) {}{}\n",
                                        if o.selected { "▶" } else { " " },
                                        i + 1,
                                        o.text,
                                        if o.free_text { "  [aceita resposta escrita]" } else { "" }
                                    ));
                                }
                                out.push_str(
                                    "(▶ = selecionada agora · cli_choose escolhe outra)",
                                );
                                Ok(out)
                            }
                            None => Ok(format!(
                                "a CLI \"{}\" não está mostrando um menu de escolha — \
                                 use cli_status para ver a tela",
                                cmd.cli_name
                            )),
                        },
                        _ => Err(format!("CLI \"{}\" não encontrada", cmd.cli_name)),
                    },
                    None => Err(format!("nenhuma CLI chamada \"{}\"", cmd.cli_name)),
                },
                "choose" => match self.find_cli(&cmd.cli_name) {
                    Some((wi, pi)) => {
                        let payload: serde_json::Value =
                            serde_json::from_str(&cmd.payload).unwrap_or_default();
                        let opcao = payload
                            .get("opcao")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();
                        let resposta = payload
                            .get("resposta")
                            .and_then(|v| v.as_str())
                            .filter(|r| !r.trim().is_empty())
                            .map(str::to_string);
                        match self.workspaces[wi].panes.get_mut(pi) {
                            Some(Pane::Term(t)) => t
                                .choose(&opcao, resposta.as_deref())
                                .map_err(|e| format!("{e:#}")),
                            _ => Err(format!("CLI \"{}\" não encontrada", cmd.cli_name)),
                        }
                    }
                    None => Err(format!("nenhuma CLI chamada \"{}\"", cmd.cli_name)),
                },
                "key" => match self.find_cli(&cmd.cli_name) {
                    Some((wi, pi)) => {
                        let payload: serde_json::Value =
                            serde_json::from_str(&cmd.payload).unwrap_or_default();
                        let nome = payload.get("tecla").and_then(|v| v.as_str()).unwrap_or("");
                        let vezes = payload
                            .get("vezes")
                            .and_then(|v| v.as_u64())
                            .unwrap_or(1)
                            .clamp(1, 20);
                        match term::NavKey::parse(nome) {
                            Some(k) => match self.workspaces[wi].panes.get_mut(pi) {
                                Some(Pane::Term(t)) => {
                                    for _ in 0..vezes {
                                        t.send_key(k);
                                        std::thread::sleep(Duration::from_millis(120));
                                    }
                                    Ok(format!("{nome} × {vezes} enviado(s)"))
                                }
                                _ => Err(format!("CLI \"{}\" não encontrada", cmd.cli_name)),
                            },
                            None => Err(format!(
                                "tecla desconhecida: \"{nome}\" (use up, down, left, right, \
                                 enter, esc, tab, space, backspace)"
                            )),
                        }
                    }
                    None => Err(format!("nenhuma CLI chamada \"{}\"", cmd.cli_name)),
                },
                "stop" => match self.find_cli(&cmd.cli_name) {
                    Some((wi, pi)) => {
                        if let Some(Pane::Term(t)) = self.workspaces[wi].panes.get_mut(pi) {
                            t.kill();
                        }
                        self.workspaces[wi].panes.remove(pi);
                        let ws = &mut self.workspaces[wi];
                        if ws.focused >= ws.panes.len() && ws.focused > 0 {
                            ws.focused -= 1;
                        }
                        if ws.panes.is_empty() && wi == self.ws_idx {
                            self.events.push(EngineEvent::FocusChat);
                        }
                        let _ = self.store.remove_cli_state(&project, &cmd.cli_name);
                        self.save_workspace_clis();
                        Ok(format!("CLI \"{}\" encerrada", cmd.cli_name))
                    }
                    None => Err(format!("nenhuma CLI chamada \"{}\"", cmd.cli_name)),
                },
                other => Err(format!("comando de CLI desconhecido: {other}")),
            };
            match outcome {
                Ok(msg) => {
                    self.status = msg.clone();
                    let _ = self.store.finish_cli_command(&cmd.id, true, &msg);
                }
                Err(msg) => {
                    self.status = format!("CLI: {msg}");
                    let _ = self.store.finish_cli_command(&cmd.id, false, &msg);
                }
            }
        }
    }

    /// Avança as CLIs gerenciadas: detecta conclusão de turno e avisa o
    /// orquestrador; publica o estado no banco (throttled) para o `cli_status`.
    pub fn tick_clis(&mut self) {
        let project = self.project().to_string();
        let publish = self.last_cli_sync.elapsed() >= CLI_SYNC_EVERY;
        let mut finished: Vec<(String, String)> = Vec::new();
        let mut states: Vec<(String, String, String)> = Vec::new();
        for ws in &mut self.workspaces {
            for pane in &mut ws.panes {
                let Pane::Term(t) = pane else { continue };
                if let Some(tail) = t.tick() {
                    finished.push((t.name.clone(), tail));
                }
                if publish && t.managed {
                    states.push((
                        t.name.clone(),
                        t.state().label().to_string(),
                        t.screen_tail(NOTIFY_TAIL_LINES),
                    ));
                }
            }
        }
        if publish {
            self.last_cli_sync = Instant::now();
            for (name, status, screen) in states {
                let _ = self
                    .store
                    .upsert_cli_state(&project, &name, &status, &screen);
            }
        }
        for (name, tail) in finished {
            // Publica o estado final na hora (o orquestrador vai consultar).
            let _ = self
                .store
                .upsert_cli_state(&project, &name, CliState::Idle.label(), &tail);
            if !self.notify_on_done {
                self.status = format!("CLI \"{name}\" concluiu (notificação desligada).");
                continue;
            }
            if self.auto_notifications >= MAX_AUTO_NOTIFICATIONS {
                self.status = format!(
                    "CLI \"{name}\" concluiu — teto de {MAX_AUTO_NOTIFICATIONS} \
                     notificações automáticas atingido; use /auto on para religar."
                );
                self.notify_on_done = false;
                continue;
            }
            self.auto_notifications += 1;
            // Só a ÚLTIMA linha útil, cortada: despejar a tela inteira aqui
            // custava token em todo turno e enchia o transcript de ruído. Se
            // quiser detalhe, o orquestrador chama cli_status/cli_read.
            let resumo = tail
                .lines()
                .rev()
                .map(str::trim)
                .find(|l| !l.is_empty())
                .unwrap_or("(sem saída)")
                .chars()
                .take(NOTIFY_SUMMARY_CHARS)
                .collect::<String>();
            self.chat.notify_system(format!(
                "A CLI \"{name}\" concluiu a tarefa (última linha: {resumo}). \
                 Veja o resultado com cli_status \"{name}\" — ou cli_read se \
                 precisar procurar algo no histórico — e decida o próximo passo."
            ));
        }
    }

    pub fn drain_background(&mut self) {
        self.chat.drain();
        self.drain_models();
        for ws in &mut self.workspaces {
            for pane in &mut ws.panes {
                if let Pane::Agent(a) = pane {
                    a.drain();
                }
            }
        }
        // Comandos que o orquestrador enfileirou (abrir CLI, mandar prompt).
        self.poll_cli_commands();
        // Conclusão de turno das CLIs → notificação para o orquestrador.
        self.tick_clis();
        // Mensagem na fila do chat (do usuário ou notificação): vai agora que
        // o turno anterior terminou, na MESMA sessão (mantém o contexto).
        if let Some(msg) = self.chat.take_queued() {
            self.send_chat(Some(msg));
        }
        // Após drenar os eventos deste tick, decide se algum card em modo
        // autônomo deve dar o próximo passo sozinho (respeitando pausa por
        // decisão pendente e o teto de iterações).
        self.advance_autonomous();
        // Persiste o que o chat produziu (transcript + sessão).
        self.persist_chat();
    }

    /// Executa um comando `/` do chat — o MESMO caminho na TUI e no app.
    pub fn run_command(&mut self, input: &str) -> CommandOutcome {
        let input = input.trim();
        if input == "/caps" {
            self.status = self.caps_summary();
        } else if let Some(id) = command_arg(input, &["/aprovar", "/approve"]) {
            self.resolve_by_chat(true, id.as_deref());
        } else if let Some(id) = command_arg(input, &["/negar", "/deny"]) {
            self.resolve_by_chat(false, id.as_deref());
        } else if let Some(raw) = input
            .strip_prefix("/agente ")
            .or_else(|| input.strip_prefix("/task "))
        {
            self.open_agent(raw.trim().to_string());
        } else if let Some(arg) = command_arg(input, &["/cli"]) {
            // Caminho manual do mesmo mecanismo que o orquestrador usa:
            // `/cli <nome> [comando [args...]]`.
            match arg {
                Some(rest) => {
                    let mut parts = rest.split_whitespace();
                    let name = parts.next().unwrap_or("").to_string();
                    let command = parts.next().unwrap_or("claude").to_string();
                    let args: Vec<String> = parts.map(str::to_string).collect();
                    self.ensure_project_setup();
                    match self.start_named_cli(&name, &command, &args, None) {
                        Ok(msg) => {
                            self.status = msg;
                            self.events.push(EngineEvent::FocusGrid);
                        }
                        Err(msg) => self.status = format!("CLI: {msg}"),
                    }
                }
                None => {
                    self.status =
                        "uso: /cli <nome> [comando] — ex.: /cli frontend, /cli build bash".into()
                }
            }
        } else if let Some(arg) = command_arg(input, &["/modelo", "/model"]) {
            match arg {
                // Nome livre: builds novas trazem modelos que o help não lista.
                Some(name) => self.set_chat_model(&name),
                None => {
                    self.refresh_models();
                    self.events.push(EngineEvent::ChooseModel);
                }
            }
        } else if let Some(arg) = command_arg(input, &["/provedor", "/provider"]) {
            match arg {
                Some(name) => self.choose_provider_by_name(&name),
                None => self.events.push(EngineEvent::ChooseProvider),
            }
        } else if let Some(arg) = command_arg(input, &["/pasta", "/dir"]) {
            self.set_workspace_dir(arg.as_deref().unwrap_or(""));
        } else if let Some(arg) = command_arg(input, &["/pasta-projeto"]) {
            let projeto = self.project().to_string();
            self.set_project_path(&projeto, arg.as_deref().unwrap_or(""));
        } else if command_arg(input, &["/terminal", "/shell"]).is_some() {
            self.open_shell();
        } else if let Some(arg) = command_arg(input, &["/ssh"]) {
            match arg {
                Some(nome) => self.open_ssh(&nome),
                None => {
                    let nomes: Vec<String> = self.ssh_hosts().into_iter().map(|h| h.nome).collect();
                    self.status = if nomes.is_empty() {
                        "nenhum servidor SSH cadastrado — use Conexões SSH no app".into()
                    } else {
                        format!("servidores: {} — /ssh <nome> abre um terminal", nomes.join(", "))
                    };
                }
            }
        } else if let Some(arg) = command_arg(input, &["/liberar-pasta"]) {
            self.authorize_dir(arg.as_deref().unwrap_or(""));
        } else if let Some(arg) = command_arg(input, &["/memoria-global"]) {
            let key = orchestrator_memory::store::AGENT_GLOBAL_KEY;
            match arg.as_deref() {
                Some("on") | Some("liga") => {
                    let _ = self.store.ui_set(key, "1");
                    self.status = "as IAs agora podem gravar memória GLOBAL (vale em todo projeto) sobre o que aprendem com você".into();
                }
                Some("off") | Some("desliga") => {
                    let _ = self.store.ui_delete(key);
                    self.status = "memória global volta a ser só sua — IAs gravam só no projeto".into();
                }
                _ => {
                    self.status = format!(
                        "memória global das IAs: {} (/memoria-global on|off)",
                        if self.store.agent_global_allowed() { "LIGADA" } else { "desligada" }
                    );
                }
            }
        } else if let Some(arg) = command_arg(input, &["/novo-projeto", "/new-project"]) {
            self.create_project(arg.as_deref().unwrap_or(""));
        } else if let Some(arg) = command_arg(input, &["/projeto", "/project"]) {
            self.switch_project(arg.as_deref());
        } else if let Some(arg) = command_arg(input, &["/auto"]) {
            match arg.as_deref() {
                Some("off") | Some("desliga") => {
                    self.notify_on_done = false;
                    self.status = "notificações de CLI concluída DESLIGADAS.".into();
                }
                Some("on") | Some("liga") => {
                    self.notify_on_done = true;
                    self.auto_notifications = 0;
                    self.status = "notificações de CLI concluída LIGADAS.".into();
                }
                _ => {
                    self.status = format!(
                        "notificações de CLI: {} ({}/{MAX_AUTO_NOTIFICATIONS} usadas) — \
                         /auto on|off",
                        if self.notify_on_done { "ligadas" } else { "desligadas" },
                        self.auto_notifications
                    )
                }
            }
        } else if let Some(arg) = command_arg(input, &["/responder", "/answer"]) {
            self.answer_by_chat(arg.as_deref().unwrap_or(""));
        } else if command_arg(input, &["/nova", "/new"]).is_some() {
            let project = self.project().to_string();
            let ws = self.ws_idx;
            let _ = self.store.clear_chat_transcript(&project, ws);
            // Conversa nova com todos: nenhuma ferramenta retoma a sessão antiga.
            for p in &self.providers {
                let _ = self.store.ui_delete(&provider_session_key(&project, ws, &p.name));
            }
            self.saved_session = None;
            self.chat.reset();
            self.status = "conversa nova — sessão e transcript zerados.".into();
        } else if command_arg(input, &["/ajuda", "/help"]).is_some() {
            self.events.push(EngineEvent::ShowHelp);
        } else {
            return CommandOutcome::NotCommand;
        }
        CommandOutcome::Done
    }

    /// Entrega (e esvazia) os pedidos à interface acumulados.
    pub fn take_events(&mut self) -> Vec<EngineEvent> {
        std::mem::take(&mut self.events)
    }
}

/// Caminho encurtado para exibição: `$HOME` vira `~`.
pub fn short_path(path: &std::path::Path) -> String {
    let full = path.display().to_string();
    match std::env::var("HOME") {
        Ok(home) if !home.is_empty() && full.starts_with(&home) => {
            format!("~{}", &full[home.len()..])
        }
        _ => full,
    }
}

/// Expande `~` no início de um caminho digitado pelo usuário.
pub fn expand_tilde(raw: &str) -> PathBuf {
    if let Some(rest) = raw.strip_prefix("~/").or_else(|| {
        if raw == "~" {
            Some("")
        } else {
            None
        }
    }) {
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home).join(rest);
        }
    }
    PathBuf::from(raw)
}

/// Chave do `ui_state` com a sessão do chat de um projeto.
pub fn session_key(project: &str, workspace: usize) -> String {
    format!("chat.session.{project}.{workspace}")
}

/// Como o chat do orquestrador se identifica para o hook e o servidor MCP.
pub const ORCHESTRATOR_AGENT_NAME: &str = "orquestrador";

/// Chave de "já anunciada": a pendência e quem decide.
fn seen_key(p: &PendingDecision) -> String {
    format!("{}:{}", p.id, p.reviewer)
}

/// A resposta do dono a uma pergunta: "1,3" vira as alternativas 1 e 3
/// (uma só, quando a pergunta não aceita várias); o resto é texto livre.
pub fn interpret_answer(q: &PendingDecision, texto: &str) -> String {
    let numeros: Option<Vec<usize>> = texto
        .split([',', ' ', ';'])
        .filter(|s| !s.trim().is_empty())
        .map(|s| s.trim().parse::<usize>().ok())
        .collect();
    match numeros {
        Some(ns)
            if !ns.is_empty()
                && !q.options.is_empty()
                && ns.iter().all(|n| (1..=q.options.len()).contains(n))
                && (q.multiple || ns.len() == 1) =>
        {
            ns.iter()
                .map(|n| q.options[n - 1].clone())
                .collect::<Vec<_>>()
                .join(", ")
        }
        _ => texto.trim().to_string(),
    }
}

/// Provedor que existia antes de haver escolha: as sessões dele continuam na
/// chave antiga, para ninguém perder a conversa ao atualizar.
pub const LEGACY_PROVIDER: &str = "Claude Code (CLI)";

/// Onde fica o provedor escolhido por último.
pub const CHAT_PROVIDER_KEY: &str = "chat.provider";

/// Mesma sanitização de `orchestrator_sandbox::container::sanitize` — não
/// dá para depender do crate `sandbox` só por causa disto (ele puxa
/// `tungstenite`/`url` à toa aqui), então é reduzido e mantido em sincronia
/// à mão: nome de sandbox vira chave de `ui_state` do jeito que o
/// `orchestrator-mcp` grava.
fn sandbox_sanitize(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '-' })
        .collect();
    let trimmed = cleaned.trim_matches('-').to_lowercase();
    if trimmed.is_empty() {
        "sandbox".to_string()
    } else {
        trimmed.chars().take(40).collect()
    }
}

/// Sessão do chat por projeto, workspace e provedor: a sessão de uma
/// ferramenta não abre na outra.
pub fn provider_session_key(project: &str, workspace: usize, provider: &str) -> String {
    if provider == LEGACY_PROVIDER {
        session_key(project, workspace)
    } else {
        format!("chat.session.{project}.{workspace}.{provider}")
    }
}

/// Casa `input` contra um comando de chat: devolve `Some(argumento)` quando
/// `input` é exatamente um dos nomes (`Some(None)`) ou "nome <arg>"
/// (`Some(Some(arg))`); `None` quando não é esse comando.
pub fn command_arg(input: &str, names: &[&str]) -> Option<Option<String>> {
    for name in names {
        if input == *name {
            return Some(None);
        }
        if let Some(rest) = input.strip_prefix(name) {
            if let Some(arg) = rest.strip_prefix(' ') {
                let arg = arg.trim();
                return Some(if arg.is_empty() {
                    None
                } else {
                    Some(arg.to_string())
                });
            }
        }
    }
    None
}

/// `Some(s)` se a string não for vazia após trim; senão `None`.
pub fn non_empty(s: &str) -> Option<String> {
    let t = s.trim();
    if t.is_empty() {
        None
    } else {
        Some(t.to_string())
    }
}

/// Rótulo curto das opções aplicadas a um agente (p/ status/título).
pub fn agent_options_label(o: &AgentOptions) -> String {
    let mut parts = Vec::new();
    if let Some(m) = &o.model {
        parts.push(m.clone());
    }
    if let Some(e) = &o.effort {
        parts.push(format!("effort:{e}"));
    }
    if let Some(p) = &o.permission_mode {
        parts.push(p.clone());
    }
    if let Some(f) = &o.fallback_model {
        parts.push(format!("fb:{f}"));
    }
    if let Some(b) = o.max_budget_usd {
        parts.push(format!("${b}"));
    }
    if o.permission_prompt_tool.is_some() {
        parts.push("ask-tool".into());
    }
    parts.join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Núcleo sem interface nenhuma, num projeto numa pasta temporária.
    fn engine_em(dir: &std::path::Path) -> Engine {
        let mut config = Config::default();
        config.projects.push(orchestrator_core::ProjectConfig {
            name: "nucleo".into(),
            path: dir.to_path_buf(),
            goal: String::new(),
            cli: "claude_code".into(),
        });
        let store = MemoryStore::open_in_memory().expect("banco em memória");
        Engine::new(store, &config, dir.join("memory.db"))
    }

    #[test]
    fn plain_text_is_not_a_command() {
        let dir = tempfile::tempdir().unwrap();
        let mut e = engine_em(dir.path());
        assert_eq!(e.run_command("abra uma CLI chamada frontend"), CommandOutcome::NotCommand);
        assert!(e.take_events().is_empty());
    }

    #[test]
    fn commands_that_need_the_interface_ask_through_events() {
        let dir = tempfile::tempdir().unwrap();
        let mut e = engine_em(dir.path());
        assert_eq!(e.run_command("/ajuda"), CommandOutcome::Done);
        assert_eq!(e.take_events(), [EngineEvent::ShowHelp]);
        assert_eq!(e.run_command("/modelo"), CommandOutcome::Done);
        assert_eq!(e.take_events(), [EngineEvent::ChooseModel]);
        // Eventos são entregues uma vez só.
        assert!(e.take_events().is_empty());
    }

    #[test]
    fn commands_change_engine_state_without_a_ui() {
        let dir = tempfile::tempdir().unwrap();
        let mut e = engine_em(dir.path());
        e.run_command("/modelo opus");
        assert_eq!(e.chat_model, "opus");
        e.run_command("/auto off");
        assert!(!e.notify_on_done);
        e.run_command("/cli");
        assert!(e.status.contains("uso: /cli"), "{}", e.status);
    }

    #[test]
    fn opening_a_cli_by_command_asks_the_ui_to_focus_the_grid() {
        let dir = tempfile::tempdir().unwrap();
        let mut e = engine_em(dir.path());
        assert_eq!(e.run_command("/cli shell sh"), CommandOutcome::Done);
        assert_eq!(e.workspaces[e.ws_idx].panes.len(), 1, "{}", e.status);
        assert_eq!(e.take_events(), [EngineEvent::FocusGrid]);
        e.close_focused_pane();
        assert_eq!(e.take_events(), [EngineEvent::FocusChat]);
    }

    #[test]
    fn a_cli_request_in_autonomous_mode_goes_to_the_orchestrator_not_the_owner() {
        use orchestrator_memory::store::REVIEWER_ORCHESTRATOR;
        let dir = tempfile::tempdir().unwrap();
        let mut e = engine_em(dir.path());
        let d = e
            .store
            .enqueue_decision_for("nucleo", "s", "[migrações] Bash: sqlx migrate run", REVIEWER_ORCHESTRATOR, "backend")
            .unwrap();
        e.reload();
        // Chega ao orquestrador como mensagem, com o caminho para decidir.
        let fila = e.chat.queued.clone().unwrap_or_default();
        assert!(fila.contains("backend") && fila.contains("decision_resolve"), "{fila}");
        // E não pede nada ao dono.
        assert_eq!(e.palette_context().pending_decisions, 0);
        assert!(!e.chat.transcript.iter().any(|l| l.who == "decisão"));

        // O orquestrador escalou: agora o dono é avisado, com o motivo.
        e.store.escalate_decision(&d.id, "reescreve dados de produção").unwrap();
        e.reload();
        let alerta = e.chat.transcript.iter().rev().find(|l| l.who == "decisão").unwrap();
        assert!(alerta.text.contains("reescreve dados de produção"), "{}", alerta.text);
        assert_eq!(e.palette_context().pending_decisions, 1);

        // O dono aprovou pelo chat: o orquestrador fica sabendo para avisar a CLI.
        e.chat.queued = None;
        e.run_command("/aprovar");
        let fila = e.chat.queued.clone().unwrap_or_default();
        assert!(fila.contains("aprovou") && fila.contains("backend"), "{fila}");
    }

    #[test]
    fn the_owner_answers_a_question_by_option_number() {
        let dir = tempfile::tempdir().unwrap();
        let mut e = engine_em(dir.path());
        let q = e
            .store
            .ask_owner("nucleo", "chat", "Qual banco usar?", &["Postgres".into(), "SQLite".into()], false, "orquestrador")
            .unwrap();
        e.reload();
        let alerta = e.chat.transcript.iter().rev().find(|l| l.who == "decisão").unwrap();
        assert!(alerta.text.contains("1) Postgres") && alerta.text.contains("/responder"), "{}", alerta.text);
        assert_eq!(e.palette_context().open_questions, 1);
        // /aprovar não responde pergunta.
        e.run_command(&format!("/aprovar {}", &q.id[..8]));
        assert!(e.store.get_decision(&q.id).unwrap().unwrap().status == "pending");

        e.chat.queued = None;
        e.run_command(&format!("/responder {} 2", &q.id[..8]));
        let respondida = e.store.get_decision(&q.id).unwrap().unwrap();
        assert_eq!(respondida.answer.as_deref(), Some("SQLite"));
        assert!(e.chat.queued.clone().unwrap_or_default().contains("SQLite"));
    }

    #[test]
    fn answers_map_numbers_to_options_or_stay_free_text() {
        let mut q = orchestrator_memory::store::MemoryStore::open_in_memory()
            .unwrap()
            .ask_owner("p", "s", "?", &["a".into(), "b".into(), "c".into()], true, "o")
            .unwrap();
        assert_eq!(interpret_answer(&q, "1, 3"), "a, c");
        assert_eq!(interpret_answer(&q, "4"), "4", "fora das alternativas vira texto");
        assert_eq!(interpret_answer(&q, "use o b com cache"), "use o b com cache");
        q.multiple = false;
        assert_eq!(interpret_answer(&q, "1 2"), "1 2", "escolha única não aceita duas");
    }

    #[test]
    fn switching_project_hides_the_other_projects_cards_without_killing_them() {
        let dir = tempfile::tempdir().unwrap();
        let mut e = engine_em(dir.path());
        let outro_dir = tempfile::tempdir().unwrap();
        e.projects.push("outro".into());
        e.project_paths.push(outro_dir.path().to_path_buf());
        e.project_goals.push(String::new());

        assert_eq!(e.run_command("/cli terminal sh"), CommandOutcome::Done);
        assert_eq!(e.workspaces[0].panes.len(), 1, "{}", e.status);
        e.chat.push_line("você", "mensagem só do projeto nucleo");

        e.switch_project(Some("outro"));
        assert_eq!(e.project(), "outro");
        assert!(e.workspaces[0].panes.is_empty(), "workspace do projeto novo deveria nascer vazia");
        assert!(
            !e.chat.transcript.iter().any(|l| l.text.contains("mensagem só do projeto nucleo")),
            "não pode ver o chat do outro projeto"
        );

        e.switch_project(Some("nucleo"));
        assert_eq!(e.project(), "nucleo");
        assert_eq!(e.workspaces[0].panes.len(), 1, "a CLI deveria voltar do jeito que ficou");
        assert!(
            e.chat.transcript.iter().any(|l| l.text.contains("mensagem só do projeto nucleo")),
            "a conversa do projeto deveria voltar"
        );

        // Mesmo projeto: não faz nada (nem zera a workspace à toa).
        e.switch_project(Some("nucleo"));
        assert_eq!(e.workspaces[0].panes.len(), 1);
    }

    #[test]
    fn card_launch_resolves_registered_clis_and_tells_the_hook_which_tool() {
        let dir = tempfile::tempdir().unwrap();
        let mut e = engine_em(dir.path());
        let codex = e.card_launch("backend", "codex", &[], true).unwrap();
        assert_eq!(codex.program, "codex");
        assert!(codex.env.contains(&("ORCHESTRATOR_HARNESS".into(), "codex".into())));
        assert!(codex.env.contains(&("ORCHESTRATOR_AUTONOMOUS".into(), "1".into())));
        // `ui_open` sem `workdir` precisa cair na pasta certa mesmo numa CLI
        // aberta em card — sem isto ela só herdava (ou não) o que a variável
        // GLOBAL do processo tivesse por acaso, do último chat, de outro
        // projeto qualquer.
        assert!(
            codex
                .env
                .contains(&("ORCHESTRATOR_WORKDIR".into(), e.workspace_dir().display().to_string())),
            "{:?}",
            codex.env
        );

        e.clis.push(orchestrator_core::CliSpec {
            name: "Meu Claude".into(),
            command: "claude".into(),
            args: vec!["--verbose".into()],
            env: [("ANTHROPIC_BASE_URL".to_string(), "http://z".to_string())].into(),
        });
        let meu = e.card_launch("api", "meu claude", &["--extra".into()], true).unwrap();
        assert_eq!(meu.program, "claude");
        assert_eq!(meu.args, ["--verbose", "--extra"]);
        assert!(meu.env.contains(&("ANTHROPIC_BASE_URL".into(), "http://z".into())));

        e.clis.push(orchestrator_core::CliSpec {
            name: "Sem chave".into(),
            command: "claude".into(),
            args: vec![],
            env: [("ANTHROPIC_AUTH_TOKEN".to_string(), "${ORCH_TESTE_SEM_CHAVE}".to_string())].into(),
        });
        let erro = e.card_launch("x", "Sem chave", &[], true).unwrap_err();
        assert!(erro.contains("ORCH_TESTE_SEM_CHAVE"), "{erro}");

        // Binário que não é ferramenta de IA não ganha marca nenhuma.
        let sh = e.card_launch("build", "bash", &[], true).unwrap();
        assert!(!sh.env.iter().any(|(k, _)| k == "ORCHESTRATOR_HARNESS"));
    }

    #[test]
    fn provider_command_opens_the_list_or_switches_by_name() {
        let dir = tempfile::tempdir().unwrap();
        let mut e = engine_em(dir.path());
        assert_eq!(e.run_command("/provedor"), CommandOutcome::Done);
        assert_eq!(e.take_events(), [EngineEvent::ChooseProvider]);
        // Ollama local não precisa de binário nem de chave: sempre pronto.
        e.run_command("/provider ollama");
        assert_eq!(e.chat.provider.name, "Ollama (local)");
        let aviso = &e.chat.transcript.last().unwrap().text;
        assert!(aviso.contains("agora com Ollama"), "{aviso}");
        assert_eq!(
            e.store.ui_get(CHAT_PROVIDER_KEY).unwrap().as_deref(),
            Some("Ollama (local)")
        );
    }

    /// Servidor HTTP de mentira numa thread própria: aceita UMA conexão,
    /// devolve `corpo` como resposta JSON 200, e devolve a URL base
    /// (`http://127.0.0.1:porta`) para apontar um provedor de teste nela.
    fn fake_http_server(corpo: &'static str) -> String {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            let (mut sock, _) = listener.accept().unwrap();
            let mut buf = [0u8; 4096];
            let _ = sock.read(&mut buf);
            let resposta = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{corpo}",
                corpo.len()
            );
            let _ = sock.write_all(resposta.as_bytes());
        });
        format!("http://{addr}")
    }

    /// Espera até `f` ficar verdadeiro, ou desiste (usado para os fetches em
    /// segundo plano de `refresh_models`/`test_model`).
    fn wait_until(mut f: impl FnMut() -> bool, ms: u64) -> bool {
        for _ in 0..(ms / 20) {
            if f() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        false
    }

    #[test]
    fn refresh_models_fetches_and_caches_the_providers_live_list() {
        let dir = tempfile::tempdir().unwrap();
        let mut e = engine_em(dir.path());
        let url = fake_http_server(r#"{"data":[{"id":"b-modelo"},{"id":"a-modelo"}]}"#);
        let idx = e.providers.iter().position(|p| p.name == "Groq").unwrap();
        e.providers[idx].base_url = url;
        e.providers[idx].api_key_env.clear(); // sem chave: não trava no fetch
        e.set_chat_provider(idx);

        assert!(e.live_models.is_empty());
        e.refresh_models();
        assert!(e.models_loading(), "deveria estar buscando");
        assert!(
            wait_until(|| { e.drain_models(); !e.models_loading() }, 2_000),
            "o fetch deveria terminar"
        );
        // Ordenados: a lista bruta veio b, a — o seletor mostra em ordem.
        let opcoes = e.model_options();
        assert_eq!(&opcoes[1..], ["a-modelo", "b-modelo"]);

        // Enquanto UM fetch está em andamento, outro pedido não empilha
        // outra busca em cima dele.
        e.refresh_models();
        e.models_loading = Some("Groq".into()); // simula fetch em andamento
        e.refresh_models();
        assert_eq!(e.models_loading.as_deref(), Some("Groq"));
    }

    #[test]
    fn refresh_models_does_nothing_for_a_provider_without_a_models_api() {
        let dir = tempfile::tempdir().unwrap();
        let mut e = engine_em(dir.path());
        e.run_command("/provedor claude code (cli)");
        e.refresh_models();
        assert!(!e.models_loading());
        assert!(e.live_models.is_empty());
    }

    #[test]
    fn test_model_reports_the_reply_and_rejects_non_http_providers() {
        let dir = tempfile::tempdir().unwrap();
        let mut e = engine_em(dir.path());
        let url = fake_http_server(r#"{"choices":[{"message":{"role":"assistant","content":"ok, processei"}}]}"#);
        let idx = e.providers.iter().position(|p| p.name == "Groq").unwrap();
        e.providers[idx].base_url = url;
        e.providers[idx].api_key_env.clear();
        e.set_chat_provider(idx);

        e.test_model("llama-3.3-70b-versatile");
        // Estado imediato: "testando…", sem travar a chamada.
        assert!(e.last_model_test.as_ref().unwrap().1.as_ref().unwrap_err().contains("testando"));
        assert!(wait_until(
            || {
                e.drain_models();
                e.last_model_test.as_ref().is_some_and(|(_, r)| r.is_ok())
            },
            2_000
        ));
        let (chave, resultado) = e.last_model_test.take().unwrap();
        assert_eq!(chave, "Groq/llama-3.3-70b-versatile");
        assert!(resultado.unwrap().contains("ok, processei"));

        // Provedor sem API HTTP: recusa na hora, sem tentar nada.
        e.run_command("/provedor claude code (cli)");
        e.test_model("opus");
        let (_, erro) = e.last_model_test.as_ref().unwrap();
        assert!(erro.as_ref().unwrap_err().contains("HTTP"), "{erro:?}");
    }

    #[test]
    fn provider_without_its_key_is_not_chosen_and_says_why() {
        let dir = tempfile::tempdir().unwrap();
        let mut e = engine_em(dir.path());
        let mut sem_chave = e.providers.iter().find(|p| p.name == "Groq").unwrap().clone();
        sem_chave.name = "Teste sem chave".into();
        sem_chave.api_key_env = "ORCH_TESTE_CHAVE_QUE_NAO_EXISTE".into();
        e.providers.push(sem_chave);
        let antes = e.chat.provider.name.clone();
        e.run_command("/provedor teste sem chave");
        assert_eq!(e.chat.provider.name, antes);
        assert!(e.status.contains("ORCH_TESTE_CHAVE_QUE_NAO_EXISTE"), "{}", e.status);
    }

    #[test]
    fn ambiguous_or_unknown_provider_names_are_explained() {
        let dir = tempfile::tempdir().unwrap();
        let mut e = engine_em(dir.path());
        // "Kimi Code" e "Kimi · OpenCode".
        e.run_command("/provedor kimi");
        assert!(e.status.contains("mais de um"), "{}", e.status);
        e.run_command("/provedor nada-disso");
        assert!(e.status.contains("nenhum provedor"), "{}", e.status);
    }

    #[test]
    fn each_provider_keeps_its_own_session() {
        let dir = tempfile::tempdir().unwrap();
        let mut e = engine_em(dir.path());
        let claude = e.provider_index();
        let ollama = e.providers.iter().position(|p| p.name == "Ollama (local)").unwrap();
        e.chat.session_id = Some("sessao-claude".into());
        e.persist_chat();
        e.set_chat_provider(ollama);
        assert!(e.chat.session_id.is_none());
        e.chat.session_id = Some("sessao-ollama".into());
        e.persist_chat();
        e.set_chat_provider(claude);
        assert_eq!(e.chat.session_id.as_deref(), Some("sessao-claude"));
        // A do Claude segue na chave antiga: quem atualiza não perde a conversa.
        assert_eq!(
            e.store.ui_get(&session_key("nucleo", 0)).unwrap().as_deref(),
            Some("sessao-claude")
        );
        // /nova apaga a sessão de todos.
        e.run_command("/nova");
        assert!(e.store.ui_get(&session_key("nucleo", 0)).unwrap().is_none());
        let chave_ollama = provider_session_key("nucleo", 0, "Ollama (local)");
        assert!(e.store.ui_get(&chave_ollama).unwrap().is_none());
    }
}
