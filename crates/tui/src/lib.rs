//! TUI do Orchestrator (ratatui).
//!
//! Duas telas:
//!
//! **Workbench** (padrão) — card de chat com o orquestrador à ESQUERDA
//! (provedor escolhível: Claude CLI, Ollama, LM Studio, Groq, OpenRouter,
//! Perplexity, NVIDIA...) e, à direita, 4 workspaces alternáveis, cada um
//! com uma GRADE de até 8 cards de três tipos:
//!   - CLIs REAIS dirigidas pelo orquestrador (ele abre com `cli_start`,
//!     manda prompt com `cli_send` e é notificado quando cada uma conclui);
//!   - terminais PTY manuais do usuário (Ctrl+t);
//!   - agentes headless (`/agente <tarefa>`, subprocesso stream-json).
//!
//! **Gerenciamento** (F2) — decisões pendentes (`a` aprova, `d` nega),
//! memórias do projeto e auditoria.

mod keys;
mod memory_panel;

pub use orchestrator_engine::term;
use orchestrator_engine::workbench::*;
use orchestrator_engine::palette;

use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result};
use crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEvent, KeyEventKind,
    KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use crossterm::execute;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Tabs, Wrap};
use ratatui::Frame;
use tui_term::widget::PseudoTerminal;

use orchestrator_core::Config;
use orchestrator_memory::store::MemoryStore;

use keys::key_to_bytes;
use term::CliState;

const TICK: Duration = Duration::from_millis(100);
/// Largura inicial do chat, em % da tela (o usuário reclamou que 35% era
/// grande demais). Ctrl+←/→ ajusta, Ctrl+b esconde.
const DEFAULT_CHAT_PCT: u16 = 25;
const MIN_CHAT_PCT: u16 = 15;
const MAX_CHAT_PCT: u16 = 60;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Screen {
    Workbench,
    Manage,
    /// Overlay de atalhos e comandos (F1).
    Help,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Focus {
    Chat,
    Terminal,
}

/// Popup aberto (índice selecionado).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Picker {
    Cli(usize),
    Provider(usize),
    /// Modelo do provedor ativo (Ctrl+m).
    Model(usize),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tab {
    Decisions,
    Memories,
    Audit,
    /// O que o orquestrador executou por conta própria (sandbox/navegador).
    Tools,
}

impl Tab {
    fn next(self) -> Self {
        match self {
            Tab::Decisions => Tab::Memories,
            Tab::Memories => Tab::Audit,
            Tab::Audit => Tab::Tools,
            Tab::Tools => Tab::Decisions,
        }
    }
}

/// A TUI: o estado de desenho por cima do [`Engine`] (acessível direto por
/// `Deref`, então `app.chat`, `app.workspaces` etc. seguem funcionando).
struct App {
    engine: Engine,
    screen: Screen,
    focus: Focus,
    picker: Option<Picker>,
    /// Painel de memória (Ctrl+Shift+W · F4), quando aberto.
    memory_panel: Option<memory_panel::MemoryPanel>,
    // --- gerenciamento ---
    tab: Tab,
    selected: ListState,
    /// Largura do painel de chat, em % da tela (Ctrl+←/→ ajusta).
    chat_pct: u16,
    /// Chat escondido? (Ctrl+b) — a grade ocupa a tela inteira.
    chat_hidden: bool,
    /// Captura de mouse ligada? (F3) — desligue para selecionar texto.
    mouse_on: bool,
    /// Áreas clicáveis do último desenho (hit-testing do mouse).
    hit: HitAreas,
    /// Rolagem do manual (ele cresceu além da tela).
    help_scroll: u16,
    /// Item destacado na paleta de comandos (quando ela está aberta).
    palette_idx: usize,
    /// Mostrar mensagens compridas por inteiro? (Ctrl+e) Por padrão elas
    /// ficam colapsadas: saída de terminal enche a tela e não é o que o
    /// usuário quer ver o tempo todo.
    expand_chat: bool,
    /// Arrastando o divisor entre chat e grade?
    dragging_divider: bool,
    /// Largura total da tela no último desenho (base do arraste).
    last_width: u16,
}

/// Retângulos do último frame, para o mouse saber no que o usuário clicou.
#[derive(Default, Clone)]
struct HitAreas {
    chat: Rect,
    /// Uma área por card da grade, na ordem dos panes.
    cards: Vec<Rect>,
    /// Abas de workspace na barra superior direita.
    ws_tabs: Vec<Rect>,
    /// Coluna divisória entre chat e grade (arrastar redimensiona).
    divider: Rect,
}

impl App {
    fn new(store: MemoryStore, config: &Config, db_path: PathBuf) -> Self {
        let mut app = Self {
            engine: Engine::new(store, config, db_path),
            screen: Screen::Workbench,
            focus: Focus::Chat,
            picker: None,
            memory_panel: None,
            tab: Tab::Decisions,
            selected: ListState::default(),
            chat_pct: DEFAULT_CHAT_PCT,
            chat_hidden: false,
            mouse_on: true,
            hit: HitAreas::default(),
            help_scroll: 0,
            palette_idx: 0,
            expand_chat: false,
            dragging_divider: false,
            last_width: 80,
        };
        app.restore_layout();
        app.clamp_selection();
        app
    }

    /// Layout escolhido antes (largura/esconder chat).
    fn restore_layout(&mut self) {
        if let Ok(Some(v)) = self.store.ui_get("chat.pct") {
            if let Ok(pct) = v.parse::<u16>() {
                self.chat_pct = pct.clamp(MIN_CHAT_PCT, MAX_CHAT_PCT);
            }
        }
        if let Ok(Some(v)) = self.store.ui_get("chat.hidden") {
            self.chat_hidden = v == "1";
        }
    }

    /// Mantém a seleção da lista do F2 dentro do que existe.
    fn clamp_selection(&mut self) {
        let len = self.current_len();
        let sel = self.selected.selected().unwrap_or(0);
        self.selected
            .select(if len == 0 { None } else { Some(sel.min(len - 1)) });
    }

    /// Aplica o que o núcleo pediu à interface.
    fn apply_events(&mut self) {
        for event in self.engine.take_events() {
            match event {
                EngineEvent::FocusGrid => self.focus = Focus::Terminal,
                EngineEvent::FocusChat => self.focus = Focus::Chat,
                EngineEvent::ChooseModel => {
                    self.engine.refresh_models();
                    self.picker = Some(Picker::Model(0));
                }
                EngineEvent::ChooseProvider => {
                    self.picker = Some(Picker::Provider(self.engine.provider_index()))
                }
                EngineEvent::ShowHelp => self.screen = Screen::Help,
            }
        }
    }

    fn reload(&mut self) {
        self.engine.reload();
        self.clamp_selection();
    }

    fn drain_background(&mut self) {
        self.engine.drain_background();
        self.apply_events();
    }

    fn open_cli(&mut self, spec_idx: usize) {
        self.engine.open_cli(spec_idx);
        self.apply_events();
    }

    fn close_focused_pane(&mut self) {
        self.engine.close_focused_pane();
        self.apply_events();
    }

    fn refresh_notices(&mut self) {
        self.engine.refresh_notices();
        if !self.mouse_on {
            self.engine
                .notices
                .push("mouse desligado (F3 religa) — clique não troca de card".into());
        }
    }

    /// Ajusta a largura do chat em passos de 5% (Ctrl+←/→), persistindo.
    fn nudge_chat_width(&mut self, delta: i16) {
        let next = (self.chat_pct as i16 + delta).clamp(MIN_CHAT_PCT as i16, MAX_CHAT_PCT as i16);
        self.set_chat_width(next as u16);
    }

    fn set_chat_width(&mut self, pct: u16) {
        self.chat_pct = pct.clamp(MIN_CHAT_PCT, MAX_CHAT_PCT);
        let _ = self.store.ui_set("chat.pct", &self.chat_pct.to_string());
        self.status = format!("chat com {}% da largura (Ctrl+b esconde).", self.chat_pct);
    }

    /// Esconde/mostra o painel de chat (Ctrl+b), persistindo a escolha.
    fn toggle_chat(&mut self) {
        self.chat_hidden = !self.chat_hidden;
        let _ = self.store.ui_set("chat.hidden", if self.chat_hidden { "1" } else { "0" });
        if self.chat_hidden {
            if !self.workspaces[self.ws_idx].panes.is_empty() {
                self.focus = Focus::Terminal;
            }
            self.status = "chat escondido — Ctrl+b mostra de novo.".into();
        } else {
            self.focus = Focus::Chat;
            self.status = "chat visível.".into();
        }
    }

    /// Alterna o foco entre chat e grade (Tab).
    fn toggle_focus(&mut self) {
        match self.focus {
            Focus::Chat => {
                if self.workspaces[self.ws_idx].panes.is_empty() {
                    self.status =
                        "workspace vazia — Ctrl+t abre uma CLI, /cli <nome> abre nomeada.".into();
                } else {
                    self.focus = Focus::Terminal;
                }
            }
            Focus::Terminal => {
                if self.chat_hidden {
                    self.toggle_chat();
                } else {
                    self.focus = Focus::Chat;
                }
            }
        }
    }

    fn current_len(&self) -> usize {
        match self.tab {
            Tab::Decisions => self.pending.len(),
            Tab::Memories => self.memories.len(),
            Tab::Audit => self.audit.len(),
            Tab::Tools => self.tool_calls.len(),
        }
    }

    fn resolve_selected(&mut self, approved: bool) {
        if self.tab != Tab::Decisions {
            return;
        }
        let Some(i) = self.selected.selected() else {
            return;
        };
        let Some(p) = self.pending.get(i) else { return };
        if p.is_question() {
            self.status = format!(
                "é uma pergunta: responda no chat com /responder {} <número(s) ou texto>",
                &p.id[..8.min(p.id.len())]
            );
            return;
        }
        let resolution = if approved {
            "aprovado pelo usuário na TUI"
        } else {
            "negado pelo usuário na TUI"
        };
        match self.store.resolve_decision(&p.id, approved, resolution) {
            Ok(()) => {
                self.status = format!(
                    "Decisão {} {} — o agente pode retomar no próximo turno.",
                    &p.id[..8.min(p.id.len())],
                    if approved { "APROVADA" } else { "NEGADA" }
                );
            }
            Err(err) => self.status = format!("erro: {err}"),
        }
        self.reload();
    }
}

impl std::ops::Deref for App {
    type Target = Engine;
    fn deref(&self) -> &Engine {
        &self.engine
    }
}

impl std::ops::DerefMut for App {
    fn deref_mut(&mut self) -> &mut Engine {
        &mut self.engine
    }
}

/// Roda a TUI até o usuário sair com `Ctrl+q`.
pub fn run(
    config: &Config,
    db_path: Option<PathBuf>,
    config_path: Option<PathBuf>,
) -> Result<()> {
    // Sobe o serviço de memória já na abertura, sem esperar: quando o
    // primeiro prompt chegar, os modelos provavelmente já estão carregando.
    orchestrator_memory::daemon::ensure_started();
    use std::io::IsTerminal;
    if !std::io::stdout().is_terminal() {
        anyhow::bail!(
            "a TUI precisa de um terminal interativo (TTY). \
             Rode `orchestrator tui` direto em um terminal, não via pipe/automação."
        );
    }
    let path = db_path.unwrap_or_else(|| config.memory_db_path.clone());
    let store = MemoryStore::open(&path)
        .with_context(|| format!("abrindo memória em {}", path.display()))?;
    let mut app = App::new(store, config, path);
    app.config = config.clone();
    app.config_path = config_path;
    // Abrir numa pasta basta: se ela não é um projeto conhecido, adota.
    if let Some(frase) = app.adopt_current_dir() {
        app.chat.push_line(
            "sistema",
            format!(
                "Adotei esta pasta: {frase}.\nA workspace 1 já aponta para cá e a \
                 conversa é desta workspace. Diga o que quer fazer — ou `/` para ver \
                 os comandos."
            ),
        );
        app.status = format!("{frase} — Ctrl+h abre o manual");
    }

    let mut terminal = ratatui::init();
    // Protocolo de teclado do kitty: é o que permite distinguir Ctrl+Shift+W
    // de Ctrl+W (painel de memória). Terminal sem suporte segue como antes —
    // e F4 abre o mesmo painel em qualquer um.
    let teclado_estendido = crossterm::terminal::supports_keyboard_enhancement().unwrap_or(false);
    if teclado_estendido {
        let _ = execute!(
            std::io::stdout(),
            crossterm::event::PushKeyboardEnhancementFlags(
                crossterm::event::KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES
            )
        );
    }
    // Mouse: clicar troca de card/chat, roda dá scroll, arrastar o divisor
    // redimensiona. F3 desliga (aí a seleção de texto do terminal volta).
    let _ = set_mouse_capture(true);
    let result = loop {
        if app.last_reload.elapsed() >= RELOAD_EVERY {
            app.reload();
        }
        app.drain_background();
        if let Err(err) = terminal.draw(|f| draw(f, &mut app)) {
            break Err(err.into());
        }
        if event::poll(TICK)? {
            match event::read()? {
                Event::Key(key) if key.kind == KeyEventKind::Press => {
                    if handle_key(&mut app, &key) {
                        break Ok(());
                    }
                }
                Event::Mouse(m) => handle_mouse(&mut app, &m),
                Event::Paste(text) if app.screen == Screen::Workbench => match app.focus {
                    Focus::Chat => app.chat.input.push_str(&text),
                    Focus::Terminal => {
                        let ws = app.ws();
                        let idx = ws.focused;
                        if let Some(Pane::Term(t)) = ws.panes.get_mut(idx) {
                            t.write_bytes(text.as_bytes());
                        }
                    }
                },
                _ => {}
            }
        }
    };
    let _ = set_mouse_capture(false);
    if teclado_estendido {
        let _ = execute!(std::io::stdout(), crossterm::event::PopKeyboardEnhancementFlags);
    }
    ratatui::restore();
    result
}

/// Liga/desliga a captura de mouse no terminal.
fn set_mouse_capture(on: bool) -> std::io::Result<()> {
    let mut out = std::io::stdout();
    if on {
        execute!(out, EnableMouseCapture)
    } else {
        execute!(out, DisableMouseCapture)
    }
}

/// Trata eventos de mouse usando as áreas registradas no último desenho.
fn handle_mouse(app: &mut App, m: &MouseEvent) {
    if !app.mouse_on || app.screen != Screen::Workbench || app.picker.is_some() {
        return;
    }
    let pos = (m.column, m.row);
    match m.kind {
        MouseEventKind::Down(MouseButton::Left) => {
            // Divisor: começa um arraste de redimensionamento.
            if hits(app.hit.divider, pos) {
                app.dragging_divider = true;
                return;
            }
            // Aba de workspace.
            if let Some(i) = app.hit.ws_tabs.iter().position(|r| hits(*r, pos)) {
                app.switch_workspace(i);
                return;
            }
            // Card da grade: foca (é o "alternar entre CLIs com o mouse").
            if let Some(i) = app.hit.cards.iter().position(|r| hits(*r, pos)) {
                let w = app.ws_idx;
                if i < app.workspaces[w].panes.len() {
                    app.workspaces[w].focused = i;
                    app.focus = Focus::Terminal;
                }
                return;
            }
            if hits(app.hit.chat, pos) {
                app.focus = Focus::Chat;
            }
        }
        MouseEventKind::Up(MouseButton::Left) => app.dragging_divider = false,
        MouseEventKind::Drag(MouseButton::Left) => {
            if app.dragging_divider {
                // A coluna do cursor define a nova largura do chat.
                let full = app.last_width.max(1);
                let pct = (m.column as u32 * 100 / full as u32) as u16;
                app.set_chat_width(pct.clamp(MIN_CHAT_PCT, MAX_CHAT_PCT));
            }
        }
        MouseEventKind::ScrollUp => scroll_under_cursor(app, pos, 3),
        MouseEventKind::ScrollDown => scroll_under_cursor(app, pos, -3),
        _ => {}
    }
}

/// Roda do mouse: rola o chat ou o card sob o cursor.
fn scroll_under_cursor(app: &mut App, pos: (u16, u16), delta: i16) {
    if hits(app.hit.chat, pos) {
        app.chat.scroll_from_end = add_scroll(app.chat.scroll_from_end, delta);
        return;
    }
    if let Some(i) = app.hit.cards.iter().position(|r| hits(*r, pos)) {
        let w = app.ws_idx;
        match app.workspaces[w].panes.get_mut(i) {
            Some(Pane::Agent(a)) => a.scroll_from_end = add_scroll(a.scroll_from_end, delta),
            // No PTY a roda mexe no scrollback do próprio terminal.
            Some(Pane::Term(t)) => t.scroll_by(delta),
            None => {}
        }
    }
}

fn add_scroll(current: u16, delta: i16) -> u16 {
    if delta >= 0 {
        current.saturating_add(delta as u16)
    } else {
        current.saturating_sub((-delta) as u16)
    }
}

/// O ponto está dentro do retângulo?
fn hits(r: Rect, (x, y): (u16, u16)) -> bool {
    r.width > 0
        && r.height > 0
        && x >= r.x
        && x < r.right()
        && y >= r.y
        && y < r.bottom()
}

/// Trata uma tecla. Retorna `true` para sair da TUI.
fn handle_key(app: &mut App, key: &KeyEvent) -> bool {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let alt = key.modifiers.contains(KeyModifiers::ALT);

    // Popup aberto: captura tudo.
    if let Some(picker) = app.picker {
        let (sel, len) = match picker {
            Picker::Cli(s) => (s, app.clis.len()),
            Picker::Provider(s) => (s, app.providers.len()),
            Picker::Model(s) => (s, app.model_options().len()),
        };
        match key.code {
            KeyCode::Esc => app.picker = None,
            KeyCode::Up => {
                let s = sel.saturating_sub(1);
                app.picker = Some(match picker {
                    Picker::Cli(_) => Picker::Cli(s),
                    Picker::Provider(_) => Picker::Provider(s),
                    Picker::Model(_) => Picker::Model(s),
                });
            }
            KeyCode::Down => {
                let s = (sel + 1).min(len.saturating_sub(1));
                app.picker = Some(match picker {
                    Picker::Cli(_) => Picker::Cli(s),
                    Picker::Provider(_) => Picker::Provider(s),
                    Picker::Model(_) => Picker::Model(s),
                });
            }
            KeyCode::Enter => {
                app.picker = None;
                match picker {
                    Picker::Cli(_) => app.open_cli(sel),
                    Picker::Provider(_) => {
                        let pronto = app
                            .provider_list()
                            .get(sel)
                            .is_some_and(|(_, a)| a.is_ready());
                        // Confere antes: falta login abre o card de login;
                        // falta instalar ou chave só explica no rodapé.
                        app.choose_provider(sel);
                        app.apply_events();
                        // Escolhido um provedor pronto, oferece logo o modelo dele.
                        if pronto {
                            app.refresh_models();
                            if app.model_options().len() > 1 || app.models_loading() {
                                app.picker = Some(Picker::Model(0));
                            }
                        }
                    }
                    Picker::Model(_) => {
                        if let Some(m) = app.model_options().get(sel).cloned() {
                            app.set_chat_model(&m);
                        }
                    }
                }
            }
            // Manda "." ao modelo em destaque e mostra se ele processa e
            // responde — sem fechar o seletor nem trocar o modelo ativo.
            KeyCode::Char('t') if matches!(picker, Picker::Model(_)) => {
                if let Some(m) = app.model_options().get(sel).cloned() {
                    app.test_model(&m);
                }
            }
            _ => {}
        }
        return false;
    }

    // Painel de memória: Ctrl+Shift+W ou F4, de qualquer tela e ANTES de a
    // tecla chegar à CLI do card (senão ela viraria texto no PTY).
    if memory_panel::is_toggle_key(key) {
        app.memory_panel = match app.memory_panel.take() {
            Some(_) => None,
            None => Some(memory_panel::MemoryPanel::open(&app.store, app.project())),
        };
        if app.memory_panel.is_none() {
            app.reload();
        }
        return false;
    }
    if let Some(mut painel) = app.memory_panel.take() {
        let project = app.project().to_string();
        let fechar = painel.handle_key(key, &app.store, &project);
        if fechar {
            app.reload();
        } else {
            app.memory_panel = Some(painel);
        }
        return false;
    }

    // Ctrl+q: sair (chat/gerenciamento) ou voltar ao chat (terminal).
    if ctrl && key.code == KeyCode::Char('q') {
        if app.screen == Screen::Workbench && app.focus == Focus::Terminal {
            app.focus = Focus::Chat;
            return false;
        }
        return true;
    }
    // Ctrl+h (ou F1): manual + avisos. É o ÚNICO lugar onde essas coisas
    // aparecem — o rodapé fica com o essencial, sem poluir a tela.
    if (ctrl && key.code == KeyCode::Char('h')) || key.code == KeyCode::F(1) {
        app.screen = match app.screen {
            Screen::Help => Screen::Workbench,
            _ => {
                app.refresh_notices();
                app.help_scroll = 0;
                Screen::Help
            }
        };
        return false;
    }
    if app.screen == Screen::Help {
        // O manual rola (não cabe em tela pequena); só teclas explícitas
        // fecham, senão rolar sairia da tela sem querer.
        match key.code {
            KeyCode::Up => app.help_scroll = app.help_scroll.saturating_sub(1),
            KeyCode::Down => app.help_scroll = app.help_scroll.saturating_add(1),
            KeyCode::PageUp => app.help_scroll = app.help_scroll.saturating_sub(10),
            KeyCode::PageDown => app.help_scroll = app.help_scroll.saturating_add(10),
            KeyCode::Home => app.help_scroll = 0,
            _ => {
                app.screen = Screen::Workbench;
                app.help_scroll = 0;
            }
        }
        return false;
    }
    // F2 alterna workbench ↔ gerenciamento.
    if key.code == KeyCode::F(2) {
        app.screen = match app.screen {
            Screen::Workbench => Screen::Manage,
            _ => Screen::Workbench,
        };
        return false;
    }

    if app.screen == Screen::Manage {
        handle_manage_key(app, key);
        return false;
    }

    // ---- Workbench: atalhos globais (valem nos dois focos) ----
    if ctrl && key.code == KeyCode::Char('t') {
        if !app.clis.is_empty() {
            app.picker = Some(Picker::Cli(0));
        }
        return false;
    }
    // Tab alterna chat ↔ grade (o atalho mais usado — antes era Ctrl+o).
    if key.code == KeyCode::Tab && !ctrl && !alt && app.focus == Focus::Chat {
        app.toggle_focus();
        return false;
    }
    // Layout: Alt+b esconde/mostra o chat, Alt+-/+ ajusta a largura. Usamos
    // Alt (não Ctrl) porque as teclas Ctrl pertencem à CLI dentro do PTY —
    // Ctrl+b/Ctrl+w são edição de linha no shell e no Claude Code.
    if alt && key.code == KeyCode::Char('b') {
        app.toggle_chat();
        return false;
    }
    if alt && matches!(key.code, KeyCode::Char('-')) {
        app.nudge_chat_width(-5);
        return false;
    }
    if alt && matches!(key.code, KeyCode::Char('+') | KeyCode::Char('=')) {
        app.nudge_chat_width(5);
        return false;
    }
    // Alt+w fecha o card focado (alias do Alt+x já existente).
    if alt && key.code == KeyCode::Char('w') {
        app.close_focused_pane();
        return false;
    }
    // F3 liga/desliga a captura de mouse (desligada = seleção de texto do
    // terminal volta a funcionar).
    if key.code == KeyCode::F(3) {
        app.mouse_on = !app.mouse_on;
        let _ = set_mouse_capture(app.mouse_on);
        app.status = if app.mouse_on {
            "mouse LIGADO — clique troca de card, roda dá scroll, arraste o divisor.".to_string()
        } else {
            "mouse DESLIGADO — a seleção de texto do terminal volta a funcionar (F3 religa).".to_string()
        };
        return false;
    }
    if alt && key.code == KeyCode::Up {
        let destino = (app.ws_idx + WS_COUNT - 1) % WS_COUNT;
        app.switch_workspace(destino);
        return false;
    }
    if alt && key.code == KeyCode::Down {
        let destino = (app.ws_idx + 1) % WS_COUNT;
        app.switch_workspace(destino);
        return false;
    }

    match app.focus {
        Focus::Terminal => handle_terminal_key(app, key, ctrl, alt),
        Focus::Chat => handle_chat_key(app, key, ctrl, alt),
    }
    false
}

fn handle_terminal_key(app: &mut App, key: &KeyEvent, ctrl: bool, alt: bool) {
    let mut close = false;
    // Mensagem de status adiada (evita conflito com o empréstimo de `ws`).
    let mut status_msg: Option<String> = None;
    {
        let w = app.ws_idx;
        let ws = &mut app.engine.workspaces[w];
        if ws.panes.is_empty() {
            app.focus = Focus::Chat;
            return;
        }
        // Atalhos reservados da grade.
        if alt {
            match key.code {
                KeyCode::Left => {
                    ws.focused = (ws.focused + ws.panes.len() - 1) % ws.panes.len();
                    return;
                }
                KeyCode::Right => {
                    ws.focused = (ws.focused + 1) % ws.panes.len();
                    return;
                }
                KeyCode::Char('x') => {
                    app.close_focused_pane();
                    return;
                }
                KeyCode::Char(c @ '1'..='8') => {
                    let i = (c as u8 - b'1') as usize;
                    if i < ws.panes.len() {
                        ws.focused = i;
                    }
                    return;
                }
                _ => {}
            }
        }
        let idx = ws.focused;
        match ws.panes.get_mut(idx) {
            Some(Pane::Term(t)) => {
                if t.is_exited() {
                    // Processo morto: Enter fecha o card.
                    close = key.code == KeyCode::Enter;
                } else if let Some(bytes) = key_to_bytes(key) {
                    t.write_bytes(&bytes);
                }
            }
            Some(Pane::Agent(a)) => {
                // Iteração individual: digite um follow-up e Enter continua a
                // sessão (`--resume`). Enter com campo vazio fecha o card
                // concluído. Ctrl+a liga/desliga o autopilot. Ctrl+q (tratado
                // acima) volta ao chat.
                match key.code {
                    KeyCode::Char('a') if ctrl => {
                        if a.goal_done {
                            status_msg = Some(format!(
                                "{}: meta já concluída — autopilot não reativa.",
                                a.name
                            ));
                        } else {
                            a.auto = !a.auto;
                            status_msg = Some(if a.auto {
                                format!(
                                    "{}: autopilot LIGADO — itera sozinho até {} passos ou a meta.",
                                    a.name, a.max_iterations
                                )
                            } else {
                                format!("{}: autopilot DESLIGADO — iteração manual.", a.name)
                            });
                        }
                    }
                    KeyCode::Enter => {
                        if a.input.trim().is_empty() {
                            close = a.done;
                        } else if a.busy {
                            // Turno em andamento: enfileira — vai automático
                            // quando o turno terminar, na MESMA sessão
                            // (`--resume`), preservando o contexto.
                            let prompt = std::mem::take(&mut a.input);
                            a.queue_followup(prompt);
                            status_msg = Some(format!(
                                "{}: mensagem na fila — envio automático ao fim do turno atual.",
                                a.name
                            ));
                        } else {
                            let prompt = std::mem::take(&mut a.input);
                            a.send_followup(prompt);
                        }
                    }
                    KeyCode::Backspace => {
                        a.input.pop();
                    }
                    KeyCode::Char(c) if !ctrl && !alt => a.input.push(c),
                    _ => {}
                }
            }
            None => {}
        }
    }
    if let Some(msg) = status_msg {
        app.status = msg;
    }
    if close {
        app.close_focused_pane();
    }
}

fn handle_chat_key(app: &mut App, key: &KeyEvent, ctrl: bool, alt: bool) {
    match key.code {
        KeyCode::Char('p') if ctrl => {
            if !app.providers.is_empty() {
                app.picker = Some(Picker::Provider(app.provider_index()));
            }
        }
        KeyCode::Char('m') if ctrl => {
            app.refresh_models();
            app.picker = Some(Picker::Model(0));
        }
        KeyCode::Char('e') if ctrl => {
            app.expand_chat = !app.expand_chat;
            app.status = if app.expand_chat {
                "mensagens compridas abertas (Ctrl+e volta a resumir).".into()
            } else {
                "mensagens compridas resumidas (Ctrl+e abre).".to_string()
            };
        }
        // Alias histórico do Tab (foco na grade).
        KeyCode::Char('o') if ctrl => app.toggle_focus(),
        // Layout — no chat as teclas Ctrl estão livres (nenhum PTY aqui).
        KeyCode::Char('b') if ctrl => app.toggle_chat(),
        KeyCode::Left if ctrl => app.nudge_chat_width(-5),
        KeyCode::Right if ctrl => app.nudge_chat_width(5),
        // Shift+Tab: cicla a postura de permissão da PRÓXIMA /agente.
        KeyCode::BackTab => {
            app.posture = app.posture.next();
            app.status = format!(
                "postura de permissão: {} — vale para a próxima /agente (Shift+Tab cicla).",
                app.posture.label()
            );
        }
        KeyCode::Enter => {
            // Paleta aberta: Enter completa o comando destacado em vez de
            // enviar um nome pela metade ao orquestrador.
            let entradas = app.palette_entries();
            if let Some(e) = entradas.get(app.palette_idx) {
                let digitado = app.chat.input.trim();
                let exato = digitado == e.command.name
                    || e.command.aliases.contains(&digitado);
                if !exato {
                    app.chat.input = format!("{} ", e.command.name);
                    app.palette_idx = 0;
                    return;
                }
            }
            let input = app.chat.input.trim().to_string();
            // O comando roda com a caixa já limpa (como antes); se não era
            // comando, o texto volta e segue para o orquestrador.
            let digitado = std::mem::take(&mut app.chat.input);
            match app.engine.run_command(&input) {
                CommandOutcome::Done => app.apply_events(),
                CommandOutcome::NotCommand => {
                    app.chat.input = digitado;
                    app.send_chat(None);
                }
            }
        }
        KeyCode::Backspace => {
            app.chat.input.pop();
            app.palette_idx = 0;
        }
        KeyCode::PageUp => {
            app.chat.scroll_from_end = app.chat.scroll_from_end.saturating_add(3);
        }
        KeyCode::PageDown => {
            app.chat.scroll_from_end = app.chat.scroll_from_end.saturating_sub(3);
        }
        // Com a paleta aberta, ↑/↓ andam nela; senão recuperam mensagens.
        KeyCode::Up => {
            if app.palette_entries().is_empty() {
                app.chat.history_prev();
            } else {
                app.palette_idx = app.palette_idx.saturating_sub(1);
            }
        }
        KeyCode::Down => {
            let n = app.palette_entries().len();
            if n == 0 {
                app.chat.history_next();
            } else {
                app.palette_idx = (app.palette_idx + 1).min(n - 1);
            }
        }
        // Tab completa o comando destacado na paleta.
        KeyCode::Tab => {
            let entradas = app.palette_entries();
            if let Some(e) = entradas.get(app.palette_idx) {
                app.chat.input = format!("{} ", e.command.name);
                app.palette_idx = 0;
            }
        }
        KeyCode::Esc => {
            app.chat.input.clear();
        }
        KeyCode::Char(c) if !ctrl && !alt => {
            app.chat.input.push(c);
            app.palette_idx = 0;
        }
        _ => {}
    }
}

/// Rótulo de "turno em andamento" com o tempo correndo.
///
/// O usuário reclamou que a TUI dizia "pensando" o tempo todo: só chamamos de
/// pensamento o que o modelo realmente streamou como `thinking`. Sem isso, o
/// que dá para afirmar é que o turno está em andamento — e o cronômetro
/// distingue "vivo" de "travado".
fn working_label(since: Option<std::time::Instant>) -> String {
    match since.map(|t| t.elapsed().as_secs()) {
        Some(s) if s >= 60 => format!("⚙ trabalhando… {}min{:02}s", s / 60, s % 60),
        Some(s) => format!("⚙ trabalhando… {s}s"),
        None => "⚙ trabalhando…".to_string(),
    }
}

/// Corta o status para caber no rodapé sem quebrar a linha.
fn trim_status(status: &str, max: u16) -> String {
    let max = max.max(10) as usize;
    let flat = status.replace('\n', " ");
    if flat.chars().count() <= max {
        return flat;
    }
    let cut: String = flat.chars().take(max.saturating_sub(1)).collect();
    format!("{cut}…")
}

/// Quebra `text` (uma linha lógica, sem `\n`) em pedaços de no máximo
/// `width` COLUNAS DE EXIBIÇÃO (unicode-width), preferindo quebrar em espaço
/// e partindo palavras mais largas que a linha. Nunca devolve vazio.
///
/// Motivo de existir: o scroll dos cards/chat precisa contar LINHAS DE TELA
/// exatas — usar `Paragraph::wrap` com a contagem lógica clipa o fim do
/// texto (as linhas embrulhadas excedem a conta). Pré-quebrando aqui, a
/// conta é exata e nada some.
fn wrap_display(text: &str, width: u16) -> Vec<String> {
    use unicode_width::UnicodeWidthChar;
    let max = width.max(1) as usize;
    let cw = |c: char| UnicodeWidthChar::width(c).unwrap_or(0);
    let mut out: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut cur_w = 0usize;
    for (i, word) in text.split(' ').enumerate() {
        // Cada palavra além da primeira carrega o espaço separador (palavras
        // vazias = espaços consecutivos/indentação, preservados).
        let sep = usize::from(i > 0);
        let word_w: usize = word.chars().map(cw).sum();
        if cur_w + sep + word_w <= max {
            if sep == 1 {
                cur.push(' ');
            }
            cur.push_str(word);
            cur_w += sep + word_w;
            continue;
        }
        // Não coube: fecha a linha atual (o espaço morre na quebra)...
        if !cur.is_empty() {
            out.push(std::mem::take(&mut cur));
            cur_w = 0;
        }
        if word_w <= max {
            cur.push_str(word);
            cur_w = word_w;
        } else {
            // ...e parte a palavra comprida em pedaços de até `max` colunas.
            for c in word.chars() {
                let w = cw(c);
                if cur_w + w > max && !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                    cur_w = 0;
                }
                cur.push(c);
                cur_w += w;
            }
        }
    }
    out.push(cur);
    out
}

/// Empurra `text` (uma linha lógica) pré-quebrado em `width`, com `style`.
fn push_wrapped(lines: &mut Vec<Line<'static>>, text: &str, width: u16, style: Style) {
    for piece in wrap_display(text, width) {
        lines.push(Line::from(Span::styled(piece, style)));
    }
}

fn handle_manage_key(app: &mut App, key: &KeyEvent) {
    match key.code {
        KeyCode::Esc | KeyCode::Char('q') => app.screen = Screen::Workbench,
        KeyCode::Tab => {
            app.tab = app.tab.next();
            app.selected
                .select(if app.current_len() == 0 { None } else { Some(0) });
        }
        KeyCode::Left => {
            app.project_idx = (app.project_idx + app.projects.len() - 1) % app.projects.len();
            app.reload();
        }
        KeyCode::Right => {
            app.project_idx = (app.project_idx + 1) % app.projects.len();
            app.reload();
        }
        KeyCode::Up => {
            if app.current_len() > 0 {
                let i = app.selected.selected().unwrap_or(0);
                app.selected.select(Some(i.saturating_sub(1)));
            }
        }
        KeyCode::Down => {
            let len = app.current_len();
            if len > 0 {
                let i = app.selected.selected().unwrap_or(0);
                app.selected.select(Some((i + 1).min(len - 1)));
            }
        }
        KeyCode::Char('a') => app.resolve_selected(true),
        KeyCode::Char('d') => app.resolve_selected(false),
        KeyCode::Char('r') => app.reload(),
        _ => {}
    }
}

fn draw(f: &mut Frame, app: &mut App) {
    match app.screen {
        Screen::Workbench => draw_workbench(f, app),
        Screen::Manage => draw_manage(f, app),
        Screen::Help => draw_help(f, app),
    }
    if let Some(picker) = app.picker {
        draw_picker(f, app, picker);
    }
    let project = app.project().to_string();
    if let Some(painel) = app.memory_panel.as_mut() {
        memory_panel::draw(f, painel, &project);
    }
}

/// Overlay de ajuda (F1): o mapa completo de atalhos e comandos. Existe
/// porque o usuário não tinha onde consultar o que a TUI faz.
fn draw_help(f: &mut Frame, app: &App) {
    let area = f.area();
    f.render_widget(Clear, area);
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" manual + avisos (↑/↓ rola · Esc, Ctrl+h ou F1 fecha) ")
        .border_style(Style::default().fg(Color::Yellow));
    let inner = block.inner(area);
    f.render_widget(block, area);

    // Duas colunas quando dá: o manual inteiro em uma coluna só não cabe em
    // terminal normal, e o que sobrasse no fim (o estado) sumiria da vista.
    let two = inner.width >= 90;
    let (left, right) = if two {
        let [l, r] =
            Layout::horizontal([Constraint::Percentage(50), Constraint::Percentage(50)])
                .areas(inner);
        (l, Some(r))
    } else {
        (inner, None)
    };

    // -1 para não encostar na coluna vizinha.
    let lw = left.width.saturating_sub(1);
    let mut col1 = help_notices(app, lw);
    col1.extend(help_commands(lw));
    let col_w = right.map(|r| r.width).unwrap_or(left.width);
    let mut col2 = help_keys(col_w);
    col2.extend(help_state(app, col_w));

    match right {
        Some(r) => {
            render_help_column(f, left, col1, app.help_scroll);
            render_help_column(f, r, col2, app.help_scroll);
        }
        None => {
            // Coluna única: tudo em sequência, rolável.
            col1.extend(col2);
            render_help_column(f, left, col1, app.help_scroll);
        }
    }
}

/// Desenha uma coluna do manual com o deslocamento de rolagem aplicado.
fn render_help_column(f: &mut Frame, area: Rect, lines: Vec<Line<'static>>, scroll: u16) {
    let max = (lines.len() as u16).saturating_sub(area.height);
    f.render_widget(
        Paragraph::new(lines).scroll((scroll.min(max), 0)),
        area,
    );
}

fn help_title(text: &str) -> Line<'static> {
    Line::from(Span::styled(
        text.to_string(),
        Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD),
    ))
}

/// Uma linha "atalho — descrição" que respeita a largura da coluna: a
/// descrição quebra alinhada sob si mesma em vez de ser cortada.
fn help_rows(k: &str, d: &str, width: u16) -> Vec<Line<'static>> {
    const KEY_W: usize = 18;
    let key_style = Style::default().fg(Color::Cyan);
    let pad = format!(" {k:<KEY_W$} ");
    let desc_w = (width as usize).saturating_sub(pad.chars().count()).max(12);
    let wrapped = wrap_display(d, desc_w as u16);
    if wrapped.is_empty() {
        return vec![Line::from(Span::styled(pad, key_style))];
    }
    let mut out = Vec::with_capacity(wrapped.len());
    for (i, chunk) in wrapped.into_iter().enumerate() {
        let head = if i == 0 {
            Span::styled(pad.clone(), key_style)
        } else {
            Span::raw(" ".repeat(pad.chars().count()))
        };
        out.push(Line::from(vec![head, Span::raw(chunk)]));
    }
    out
}

/// Avisos do estado atual — a primeira coisa do manual, porque é o que o
/// usuário veio ver quando algo não está funcionando.
fn help_notices(app: &App, width: u16) -> Vec<Line<'static>> {
    let mut lines = vec![help_title("Avisos")];
    if app.notices.is_empty() {
        lines.push(Line::from(vec![
            Span::styled(" ✔ ", Style::default().fg(Color::Green)),
            Span::styled(
                "nada pendente.".to_string(),
                Style::default().fg(Color::DarkGray),
            ),
        ]));
    } else {
        for n in &app.notices {
            push_wrapped(
                &mut lines,
                &format!("⚠ {n}"),
                width.saturating_sub(1),
                Style::default().fg(Color::Yellow),
            );
        }
    }
    lines.push(Line::from(""));
    lines
}

fn help_commands(width: u16) -> Vec<Line<'static>> {
    let mut lines = vec![help_title("Comandos do chat")];
    for (k, d) in [
        ("/ (barra)", "abre a paleta: filtra comandos e diz o que falta"),
        ("/cli <nome>", "abre uma CLI real nomeada (ex.: /cli frontend)"),
        ("/novo-projeto", "cria projeto (sem caminho abre o explorador)"),
        ("/agente <tarefa>", "agente headless (subprocesso stream-json)"),
        ("/modelo [nome]", "modelo do chat (sem nome: abre o seletor)"),
        ("/pasta [caminho]", "pasta da workspace (vazio abre o explorador)"),
        ("/projeto [nome]", "troca o projeto (restaura a conversa dele)"),
        ("/nova", "zera conversa e sessão deste projeto"),
        ("/aprovar /negar", "resolve a decisão pendente (ou F2: a/d)"),
        ("/auto on|off", "avisar quando uma CLI concluir"),
        ("/caps", "o que a build do `claude` suporta"),
    ] {
        lines.extend(help_rows(k, d, width));
    }
    lines.push(Line::from(""));
    lines
}

fn help_keys(width: u16) -> Vec<Line<'static>> {
    let mut lines = vec![help_title("Foco, layout e telas")];
    for (k, d) in [
        ("Tab (no chat)", "vai para a grade de cards"),
        ("Ctrl+q (no card)", "volta para o chat"),
        ("clique", "foca o card/chat clicado"),
        ("Alt+1..8", "foca o card N"),
        ("Alt+←/→", "card anterior / próximo"),
        ("Alt+↑/↓", "workspace anterior / próxima (leva o chat junto)"),
        ("roda do mouse", "rola o chat ou o card sob o cursor"),
        ("Alt+b", "esconde/mostra o chat"),
        ("Alt+- / Alt++", "largura do chat (15–60%)"),
        ("Ctrl+t", "nova CLI pelo seletor"),
        ("Ctrl+p / Ctrl+m", "provedor / modelo do chat"),
        ("Alt+x / Alt+w", "fecha o card focado"),
        ("Ctrl+a", "autopilot do card de agente"),
        ("Shift+Tab", "postura de permissão da próxima /agente"),
        ("↑/↓ (no chat)", "histórico; com a paleta aberta, anda nela"),
        ("Tab (na paleta)", "completa o comando destacado"),
        ("Ctrl+e", "abre/resume as mensagens compridas do chat"),
        ("Ctrl+h / F1", "este manual"),
        ("F2", "decisões, memórias, auditoria"),
        ("F3", "liga/desliga captura de mouse"),
        ("Ctrl+Shift+W / F4", "painel de memória: global, projeto, IAs"),
        ("Ctrl+q", "sai da TUI"),
    ] {
        lines.extend(help_rows(k, d, width));
    }
    lines.push(Line::from(""));
    lines
}

fn help_state(app: &App, width: u16) -> Vec<Line<'static>> {
    let mut lines = vec![help_title("Estado")];
    let dim = Style::default().fg(Color::DarkGray);
    let _ = dim;
    for (k, v) in [
        (
            "projeto",
            format!("{} · {}", app.project(), short_path(&app.workspace_dir())),
        ),
        (
            "chat",
            format!(
                "{} · modelo {}",
                app.chat.provider.name,
                if app.chat_model.is_empty() {
                    "padrão".to_string()
                } else {
                    app.chat_model.clone()
                }
            ),
        ),
        (
            "layout",
            format!(
                "chat {}%{} · mouse {}",
                app.chat_pct,
                if app.chat_hidden { " (escondido)" } else { "" },
                if app.mouse_on { "on" } else { "off" }
            ),
        ),
        ("postura", app.posture.label().to_string()),
        (
            "memória",
            format!(
                "{} memórias · {} decisões pendentes",
                app.memories.len(),
                app.pending.len()
            ),
        ),
        (
            "CLIs",
            format!(
                "{} nesta workspace · avisar ao concluir: {}",
                app.workspaces[app.ws_idx].panes.len(),
                if app.notify_on_done { "sim" } else { "não" }
            ),
        ),
    ] {
        lines.extend(help_rows(k, &v, width));
    }
    lines
}

// ---------------------------------------------------------------- workbench

fn draw_workbench(f: &mut Frame, app: &mut App) {
    let [main, footer] =
        Layout::vertical([Constraint::Min(5), Constraint::Length(1)]).areas(f.area());
    app.last_width = main.width;
    // Chat com largura ajustável (Ctrl+←/→) e escondível (Ctrl+b).
    let right = if app.chat_hidden {
        app.hit.chat = Rect::default();
        app.hit.divider = Rect::default();
        main
    } else {
        let [chat_area, right] = Layout::horizontal([
            Constraint::Percentage(app.chat_pct),
            Constraint::Percentage(100 - app.chat_pct),
        ])
        .areas(main);
        draw_chat(f, app, chat_area);
        app.hit.chat = chat_area;
        // Coluna divisória: 1 char na borda esquerda da grade.
        app.hit.divider = Rect {
            x: right.x,
            y: right.y,
            width: 1,
            height: right.height,
        };
        right
    };

    let [wsbar, grid] =
        Layout::vertical([Constraint::Length(1), Constraint::Min(3)]).areas(right);
    draw_ws_bar(f, app, wsbar);
    draw_grid(f, app, grid);

    // Rodapé mínimo: um atalho para o manual, o estado da vez e um contador
    // de avisos. Tudo o mais (atalhos, diagnósticos) vive dentro do manual.
    let mut spans = vec![Span::styled(
        "Ctrl+h manual",
        Style::default().fg(Color::Cyan),
    )];
    if !app.notices.is_empty() {
        spans.push(Span::raw(" · "));
        spans.push(Span::styled(
            format!("⚠ {}", app.notices.len()),
            Style::default().fg(Color::Yellow),
        ));
    }
    if !app.status.is_empty() {
        spans.push(Span::raw(" · "));
        spans.push(Span::styled(
            trim_status(&app.status, footer.width.saturating_sub(24)),
            Style::default().fg(Color::DarkGray),
        ));
    }
    f.render_widget(Paragraph::new(Line::from(spans)), footer);
}

fn draw_ws_bar(f: &mut Frame, app: &mut App, area: Rect) {
    let mut spans: Vec<Span> = vec![Span::styled(
        " workspaces ",
        Style::default().fg(Color::DarkGray),
    )];
    // Posições das abas para o clique do mouse (a largura é o texto).
    app.hit.ws_tabs.clear();
    let mut x = area.x + " workspaces ".chars().count() as u16;
    let ativa = app.ws_idx;
    for (i, ws) in app.engine.workspaces.iter().enumerate() {
        let label = format!(" {} [{}] ", i + 1, ws.panes.len());
        let w = label.chars().count() as u16;
        app.hit.ws_tabs.push(Rect {
            x: x.min(area.right()),
            y: area.y,
            width: w.min(area.right().saturating_sub(x)),
            height: 1,
        });
        // +1 pelo espaço separador empurrado adiante.
        x = x.saturating_add(w + 1);
        let style = if i == ativa {
            Style::default().fg(Color::Black).bg(Color::Yellow)
        } else if ws.panes.is_empty() {
            Style::default().fg(Color::DarkGray)
        } else {
            Style::default().fg(Color::White)
        };
        spans.push(Span::styled(label, style));
        spans.push(Span::raw(" "));
    }
    // Pasta em que esta workspace abre CLIs (`/pasta <caminho>` muda).
    let dir = app.workspace_dir();
    let shown = dir
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| dir.display().to_string());
    let own = app.workspaces[app.ws_idx].dir.is_some();
    spans.push(Span::styled(
        format!("📁 {shown}{}", if own { "" } else { " (projeto)" }),
        Style::default().fg(if own { Color::Cyan } else { Color::DarkGray }),
    ));
    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

/// Divide `area` em uma grade para `n` cards (máx 8): 1, 1x2, 2x2, 2x3, 2x4.
fn grid_areas(area: Rect, n: usize) -> Vec<Rect> {
    if n == 0 {
        return Vec::new();
    }
    let (rows, cols) = match n {
        1 => (1, 1),
        2 => (1, 2),
        3 | 4 => (2, 2),
        5 | 6 => (2, 3),
        _ => (2, 4),
    };
    let row_areas = Layout::vertical(vec![Constraint::Ratio(1, rows as u32); rows]).split(area);
    let mut out = Vec::with_capacity(rows * cols);
    for r in row_areas.iter() {
        let cells = Layout::horizontal(vec![Constraint::Ratio(1, cols as u32); cols]).split(*r);
        out.extend(cells.iter().copied());
    }
    out.truncate(n);
    out
}

fn draw_grid(f: &mut Frame, app: &mut App, area: Rect) {
    let focused_grid = app.focus == Focus::Terminal;
    let ws_idx = app.ws_idx;
    // Áreas dos cards para o clique do mouse (calculadas antes do empréstimo
    // mutável dos panes).
    app.hit.cards = grid_areas(area, app.workspaces[ws_idx].panes.len());
    let ws = &mut app.workspaces[ws_idx];

    if ws.panes.is_empty() {
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::DarkGray))
            .title(format!(" Workspace {} ", ws_idx + 1));
        let inner = block.inner(area);
        f.render_widget(block, area);
        f.render_widget(
            Paragraph::new(
                "\nWorkspace vazia.\n\nPeça no chat: \"abra uma CLI chamada frontend e mande criar o README\"\n→ o ORQUESTRADOR abre a CLI real aqui, manda a tarefa e avisa quando ela concluir.\n\n/cli <nome> → abre você mesmo uma CLI nomeada · Ctrl+t → seletor de CLI\n/agente <tarefa> → agente headless · /pasta <caminho> → pasta desta workspace\nAté 8 cards por workspace · Alt+↑/↓ troca · F1 mostra todos os atalhos.",
            )
            .wrap(Wrap { trim: false })
            .style(Style::default().fg(Color::DarkGray)),
            inner,
        );
        return;
    }

    let cells = grid_areas(area, ws.panes.len());
    for (i, (pane, cell)) in ws.panes.iter_mut().zip(cells.iter()).enumerate() {
        let is_focused = focused_grid && i == ws.focused;
        let border = if is_focused { Color::Yellow } else { Color::DarkGray };
        let (title, title_color) = match pane {
            Pane::Term(t) => {
                if t.is_exited() {
                    (format!(" {} ✖ encerrado — Enter fecha ", t.name), Color::Red)
                } else if t.managed {
                    // CLI do orquestrador: mostra o que ela está fazendo.
                    match t.state() {
                        CliState::Working => (
                            format!(" {} ⚙ orquestrador ({}) ", t.name, i + 1),
                            Color::Cyan,
                        ),
                        CliState::Idle => (
                            format!(" {} ✔ ociosa ({}) ", t.name, i + 1),
                            Color::Green,
                        ),
                        _ => (
                            format!(" {} ⋯ iniciando ({}) ", t.name, i + 1),
                            Color::Yellow,
                        ),
                    }
                } else {
                    (format!(" {} ({}) ", t.name, i + 1), Color::White)
                }
            }
            Pane::Agent(a) => {
                let tag = if a.opts_label.is_empty() {
                    String::new()
                } else {
                    format!(" [{}]", a.opts_label)
                };
                // Estado do autopilot: meta concluída, autônomo em curso, ou nada.
                let auto = if a.goal_done {
                    " ✔meta".to_string()
                } else if a.auto {
                    format!(" ⟳auto {}/{}", a.iterations, a.max_iterations)
                } else {
                    String::new()
                };
                if a.busy {
                    (
                        format!(" {}{tag}{auto} ⚙ trabalhando ({}) ", a.name, i + 1),
                        Color::Cyan,
                    )
                } else if a.done {
                    let mark = if a.is_error { "✖" } else { "✔" };
                    let color = if a.is_error { Color::Red } else { Color::Green };
                    let hint = if a.goal_done {
                        "meta concluída — Enter fecha"
                    } else if a.auto {
                        "autopilot ligado (Ctrl+a desliga)"
                    } else {
                        "digite p/ iterar · Ctrl+a autopilot"
                    };
                    (format!(" {} {mark}{tag}{auto} — {hint} ", a.name), color)
                } else {
                    (
                        format!(" {}{tag}{auto} ⚙ orquestrador ({}) ", a.name, i + 1),
                        Color::Cyan,
                    )
                }
            }
        };
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(border))
            .title(Span::styled(title, Style::default().fg(title_color)));
        let inner = block.inner(*cell);

        match pane {
            Pane::Term(t) => {
                t.resize(inner.height, inner.width);
                let guard = t.parser();
                let widget = PseudoTerminal::new(guard.screen()).block(block);
                f.render_widget(widget, *cell);
            }
            Pane::Agent(a) => {
                f.render_widget(block, *cell);
                // Tudo pré-quebrado na largura do card: o scroll conta linhas
                // DE TELA exatas — nada é clipado no fim (bug de wrap).
                let width = inner.width;
                let dim = Style::default().fg(Color::DarkGray);
                let plain = Style::default();
                let mut lines: Vec<Line> = Vec::new();
                push_wrapped(&mut lines, &format!("tarefa: {}", a.task), width, plain);
                lines.push(Line::from(""));
                for l in a.output.lines() {
                    push_wrapped(&mut lines, l, width, plain);
                }
                if a.busy {
                    // Pensamento do turn em andamento, esmaecido. Sem
                    // thinking streamado, mostramos TRABALHANDO com o tempo
                    // correndo — dizer "pensando" o tempo todo dá a impressão
                    // errada (e não distingue vivo de travado).
                    if a.thinking.is_empty() {
                        push_wrapped(&mut lines, &working_label(a.busy_since), width, dim);
                    } else {
                        for l in a.thinking.lines() {
                            push_wrapped(&mut lines, &format!("💭 {l}"), width, dim);
                        }
                    }
                }
                // Rodapé de consumo: tokens ao vivo + custo quando reportado.
                if a.tokens_in > 0 || a.tokens_out > 0 || a.cost.is_some() {
                    let mut usage = format!("⟳ {}→{} tok", a.tokens_in, a.tokens_out);
                    if let Some(c) = a.cost {
                        usage.push_str(&format!(" · ${c:.4}"));
                    }
                    push_wrapped(&mut lines, &usage, width, dim);
                }
                // Follow-up já enfileirado (Enter durante o turno).
                if let Some(q) = &a.queued {
                    push_wrapped(&mut lines, &format!("⏳ na fila: {q}"), width, dim);
                }
                if is_focused {
                    // Linha de follow-up SEMPRE visível no card focado:
                    // ocioso envia direto; ocupado enfileira (Enter).
                    lines.push(Line::from(""));
                    let marker = if a.busy { "▸ (fila) " } else { "▸ " };
                    push_wrapped(
                        &mut lines,
                        &format!("{marker}{}▌", a.input),
                        width,
                        plain,
                    );
                } else if a.busy {
                    lines.push(Line::from("▌"));
                }
                let line_count = lines.len() as u16;
                let bottom = line_count.saturating_sub(inner.height);
                // Roda do mouse desloca a partir do fim (0 = acompanhando).
                let scroll = bottom.saturating_sub(a.scroll_from_end.min(bottom));
                // Sem `.wrap()`: as linhas já cabem na largura, e re-embrulhar
                // quebraria a conta exata do scroll.
                f.render_widget(Paragraph::new(lines).scroll((scroll, 0)), inner);
            }
        }
    }
}

fn draw_chat(f: &mut Frame, app: &mut App, area: Rect) {
    let focused = app.focus == Focus::Chat;
    let border = if focused { Color::Yellow } else { Color::DarkGray };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(border))
        .title(format!(
            " Orchestrator [{}] — {} · {} ",
            app.chat.provider.name,
            app.project(),
            app.posture.label()
        ));
    let inner = block.inner(area);
    f.render_widget(block, area);

    let [log_area, input_area] =
        Layout::vertical([Constraint::Min(3), Constraint::Length(2)]).areas(inner);

    // Tudo pré-quebrado na largura do log: o scroll conta linhas DE TELA
    // exatas — mensagens compridas não são mais clipadas (bug de wrap).
    let width = log_area.width;
    let dim = Style::default().fg(Color::DarkGray);
    let plain = Style::default();
    let mut lines: Vec<Line> = Vec::new();
    for entry in &app.chat.transcript {
        let (label, color) = match entry.who.as_str() {
            "você" => ("você", Color::Cyan),
            "erro" => ("erro", Color::Red),
            "decisão" => ("decisão", Color::Yellow),
            "sistema" => ("sistema", Color::Magenta),
            _ => ("orchestrator", Color::Green),
        };
        lines.push(Line::from(Span::styled(
            format!("● {label}"),
            Style::default().fg(color).add_modifier(Modifier::BOLD),
        )));
        // Mensagens compridas (saída de terminal, sobretudo) ficam
        // resumidas até o usuário pedir para ver — Ctrl+e alterna.
        let shown = if !app.expand_chat && entry.is_long() {
            entry.preview()
        } else {
            entry.text.clone()
        };
        for l in shown.lines() {
            let style = if l.starts_with("… +") { dim } else { plain };
            push_wrapped(&mut lines, l, width, style);
        }
        lines.push(Line::from(""));
    }
    if app.chat.busy {
        // Contador de tokens ao vivo junto do "digitando…".
        let mut label = "● orchestrator (digitando…".to_string();
        if app.chat.tokens_in > 0 || app.chat.tokens_out > 0 {
            label.push_str(&format!(
                " · {}→{} tok",
                app.chat.tokens_in, app.chat.tokens_out
            ));
        }
        label.push(')');
        lines.push(Line::from(Span::styled(
            label,
            Style::default().fg(Color::Green).add_modifier(Modifier::BOLD),
        )));
        // Pensamento parcial esmaecido, acima da resposta que vai chegando;
        // quando o modelo pensa sem streamar nada, um marcador de vida.
        if app.chat.thinking().is_empty() && app.chat.partial().is_empty() {
            push_wrapped(&mut lines, &working_label(app.chat.busy_since), width, dim);
        }
        for l in app.chat.thinking().lines() {
            push_wrapped(&mut lines, &format!("💭 {l}"), width, dim);
        }
        for l in app.chat.partial().lines() {
            push_wrapped(&mut lines, l, width, plain);
        }
    }
    if lines.is_empty() {
        lines.push(Line::from(Span::styled(
            "Converse com o orquestrador sobre o projeto.",
            Style::default().fg(Color::DarkGray),
        )));
        lines.push(Line::from(Span::styled(
            "Ctrl+p troca o provedor de LLM.",
            Style::default().fg(Color::DarkGray),
        )));
        lines.push(Line::from(Span::styled(
            "/agente <tarefa> põe um agente para trabalhar na grade.",
            Style::default().fg(Color::DarkGray),
        )));
    }
    let total = lines.len() as u16;
    let visible = log_area.height;
    let bottom = total.saturating_sub(visible);
    let scroll = bottom.saturating_sub(app.chat.scroll_from_end.min(bottom));
    // Sem `.wrap()`: as linhas já cabem na largura (conta exata do scroll).
    f.render_widget(Paragraph::new(lines).scroll((scroll, 0)), log_area);

    // Ocupado: o Enter enfileira em vez de perder a mensagem — o prompt diz.
    let prompt = if app.chat.busy { "(fila) ❯" } else { "❯" };
    let input = Line::from(vec![
        Span::styled(format!("{prompt} "), Style::default().fg(Color::Yellow)),
        Span::raw(app.chat.input.as_str()),
    ]);
    // Totais do último turn no separador do input (persistem após concluir).
    let mut input_block = Block::default().borders(Borders::TOP);
    if let Some(q) = &app.chat.queued {
        let first = q.lines().next().unwrap_or("").to_string();
        let extra = q.lines().count().saturating_sub(1);
        let mut label = format!(" ⏳ na fila: {first}");
        if extra > 0 {
            label.push_str(&format!(" (+{extra})"));
        }
        label.push(' ');
        input_block = input_block.title(Span::styled(
            label,
            Style::default().fg(Color::Yellow),
        ));
    }
    if app.chat.tokens_in > 0 || app.chat.tokens_out > 0 {
        let mut usage = format!(" ⟳ {}→{} tok", app.chat.tokens_in, app.chat.tokens_out);
        if let Some(c) = app.chat.cost {
            usage.push_str(&format!(" · ${c:.4}"));
        }
        usage.push(' ');
        input_block = input_block.title(Span::styled(
            usage,
            Style::default().fg(Color::DarkGray),
        ));
    }
    f.render_widget(Paragraph::new(input).block(input_block), input_area);

    // Paleta de comandos: aparece sobre o log enquanto o nome é digitado.
    draw_palette(f, app, log_area);
    if focused && app.picker.is_none() {
        let prompt_w = prompt.chars().count() as u16 + 1;
        let x = input_area.x + prompt_w + app.chat.input.chars().count() as u16;
        f.set_cursor_position((x.min(input_area.right().saturating_sub(1)), input_area.y + 1));
    }
}

/// Lista de comandos que casam o que está sendo digitado, com o estado de
/// cada um — é o que responde "o que existe?" e "por que não funcionou?".
fn draw_palette(f: &mut Frame, app: &App, area: Rect) {
    let entradas = app.palette_entries();
    if entradas.is_empty() {
        // Digitou `/` e nada casou: diz isso, em vez de não mostrar nada.
        if palette::should_open(&app.chat.input) && app.chat.input.len() > 1 {
            let aviso = Paragraph::new(format!(
                "nenhum comando com \"{}\" — Esc limpa, Ctrl+h abre o manual",
                app.chat.input.trim_start_matches('/')
            ))
            .style(Style::default().fg(Color::DarkGray))
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(" comandos ")
                    .border_style(Style::default().fg(Color::DarkGray)),
            );
            let caixa = palette_area(area, 3);
            f.render_widget(Clear, caixa);
            f.render_widget(aviso, caixa);
        }
        return;
    }

    // O chat pode estar estreito (25% da tela): aí cada item vira só o nome,
    // e o detalhe do item selecionado ganha uma linha própria no rodapé —
    // espremer uso + descrição + motivo numa coluna de 25 colunas não informa
    // nada.
    let estreito = area.width < 62;
    let extra = if estreito { 3 } else { 2 };
    let altura = (entradas.len() as u16 + extra)
        .min(area.height.saturating_sub(1))
        .max(4);
    let caixa = palette_area(area, altura);
    let visiveis = caixa.height.saturating_sub(extra) as usize;
    let inicio = app.palette_idx.saturating_sub(visiveis.saturating_sub(1));

    let mut linhas: Vec<Line> = entradas
        .iter()
        .enumerate()
        .skip(inicio)
        .take(visiveis)
        .map(|(i, e)| {
            let marcado = i == app.palette_idx;
            let (marca, cor) = match &e.readiness {
                palette::Readiness::Ready => ("●", Color::Green),
                palette::Readiness::NeedsArgument(_) => ("○", Color::Yellow),
                palette::Readiness::Unavailable(_) => ("✖", Color::Red),
            };
            let estilo_nome = if marcado {
                Style::default().fg(Color::Black).bg(Color::Cyan)
            } else {
                Style::default().fg(Color::Cyan)
            };
            let mut spans = vec![
                Span::styled(format!(" {marca} "), Style::default().fg(cor)),
                Span::styled(
                    if estreito {
                        e.command.name.to_string()
                    } else {
                        format!("{:<18}", e.command.usage)
                    },
                    estilo_nome,
                ),
            ];
            if !estreito {
                spans.push(Span::raw(" "));
                spans.push(Span::styled(
                    e.command.about.to_string(),
                    Style::default().fg(Color::Gray),
                ));
                if !matches!(e.readiness, palette::Readiness::Ready) {
                    spans.push(Span::styled(
                        format!("  — {}", e.readiness.label()),
                        Style::default().fg(cor),
                    ));
                }
            }
            Line::from(spans)
        })
        .collect();

    // Detalhe do selecionado, para a coluna estreita não esconder o motivo.
    if estreito {
        if let Some(e) = entradas.get(app.palette_idx) {
            let (texto, cor) = match &e.readiness {
                palette::Readiness::Ready => (e.command.usage.to_string(), Color::Gray),
                palette::Readiness::NeedsArgument(m) => {
                    (format!("{} — {m}", e.command.usage), Color::Yellow)
                }
                palette::Readiness::Unavailable(m) => (m.clone(), Color::Red),
            };
            push_wrapped(&mut linhas, &texto, caixa.width.saturating_sub(2), Style::default().fg(cor));
        }
    }

    let titulo = if estreito {
        format!(" {}/{} · Tab ", app.palette_idx + 1, entradas.len())
    } else {
        format!(
            " comandos ({}/{}) · ↑↓ escolhe · Tab completa ",
            app.palette_idx + 1,
            entradas.len()
        )
    };
    f.render_widget(Clear, caixa);
    f.render_widget(
        Paragraph::new(linhas).block(
            Block::default()
                .borders(Borders::ALL)
                .title(titulo)
                .border_style(Style::default().fg(Color::Cyan)),
        ),
        caixa,
    );
}

/// Caixa da paleta: ancorada embaixo do log, junto do campo de digitação.
fn palette_area(area: Rect, altura: u16) -> Rect {
    let altura = altura.min(area.height);
    Rect {
        x: area.x,
        y: area.y + area.height.saturating_sub(altura),
        width: area.width,
        height: altura,
    }
}

fn draw_picker(f: &mut Frame, app: &App, picker: Picker) {
    let (title, sel, items): (String, usize, Vec<ListItem>) = match picker {
        Picker::Cli(sel) => (
            " Abrir CLI (↑/↓, Enter, Esc) ".to_string(),
            sel,
            app.clis
                .iter()
                .map(|c| ListItem::new(format!("{}  ({})", c.name, c.command)))
                .collect(),
        ),
        Picker::Provider(sel) => (
            " Quem responde no chat (● pronto · ○ falta entrar na conta · ✖ falta instalar/chave) "
                .to_string(),
            sel,
            app.provider_list()
                .into_iter()
                .map(|(p, a)| {
                    let atual = if p.name == app.chat.provider.name { "▸" } else { " " };
                    // Pronto: a ferramenta e o modelo. Senão: o que falta.
                    let detalhe = if a.is_ready() {
                        if p.model.is_empty() {
                            p.kind.tool_label().to_string()
                        } else {
                            format!("{} · {}", p.kind.tool_label(), p.model)
                        }
                    } else {
                        a.hint.clone()
                    };
                    ListItem::new(format!("{atual} {} {}  — {detalhe}", a.state.glyph(), p.name))
                })
                .collect(),
        ),
        Picker::Model(sel) => (
            format!(
                " Modelo de {} (↑/↓, Enter, Esc, t testa · ou /modelo <nome>){} ",
                app.chat.provider.name,
                if app.models_loading() { " — sincronizando com a API…" } else { "" }
            ),
            sel,
            app.model_options()
                .iter()
                .map(|m| {
                    let marca = if *m == DEFAULT_MODEL_LABEL && app.chat_model.is_empty()
                        || *m == app.chat_model
                    {
                        "● "
                    } else {
                        "  "
                    };
                    // O resultado do último teste fica junto do modelo testado.
                    let teste = app
                        .last_model_test
                        .as_ref()
                        .filter(|(chave, _)| chave == &format!("{}/{m}", app.chat.provider.name))
                        .map(|(_, r)| match r {
                            Ok(ok) => format!("  ✔ {ok}"),
                            Err(e) if e == "testando…" => "  … testando".to_string(),
                            Err(e) => format!("  ✖ {e}"),
                        })
                        .unwrap_or_default();
                    ListItem::new(format!("{marca}{m}{teste}"))
                })
                .collect(),
        ),
    };
    let title: &str = &title;
    let area = f.area();
    // A lista de provedores diz o que falta em cada um: precisa de espaço.
    let largura = if matches!(picker, Picker::Provider(_)) { 110 } else { 56 };
    let w = largura.min(area.width.saturating_sub(4));
    let h = (items.len() as u16 + 2).min(area.height.saturating_sub(4));
    let popup = Rect {
        x: area.x + (area.width.saturating_sub(w)) / 2,
        y: area.y + (area.height.saturating_sub(h)) / 2,
        width: w,
        height: h,
    };
    f.render_widget(Clear, popup);
    let mut state = ListState::default();
    state.select(Some(sel));
    let list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(title)
                .border_style(Style::default().fg(Color::Yellow)),
        )
        .highlight_style(Style::default().bg(Color::Yellow).fg(Color::Black))
        .highlight_symbol("▶ ");
    f.render_stateful_widget(list, popup, &mut state);
}

// ------------------------------------------------------------ gerenciamento

fn draw_manage(f: &mut Frame, app: &mut App) {
    let [header, body, detail, footer] = Layout::vertical([
        Constraint::Length(3),
        Constraint::Min(5),
        Constraint::Length(7),
        Constraint::Length(1),
    ])
    .areas(f.area());

    let titles = vec![
        format!("Decisões pendentes ({})", app.pending.len()),
        format!("Memórias ({})", app.memories.len()),
        format!("Auditoria ({})", app.audit.len()),
        format!("Ferramentas ({})", app.tool_calls.len()),
    ];
    let idx = match app.tab {
        Tab::Decisions => 0,
        Tab::Memories => 1,
        Tab::Audit => 2,
        Tab::Tools => 3,
    };
    let tabs = Tabs::new(titles)
        .select(idx)
        .highlight_style(Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(format!(" Gerenciamento — projeto: {} (←/→) ", app.project())),
        );
    f.render_widget(tabs, header);

    match app.tab {
        Tab::Decisions => draw_decisions(f, app, body, detail),
        Tab::Memories => draw_memories(f, app, body, detail),
        Tab::Audit => draw_audit(f, app, body, detail),
        Tab::Tools => draw_tools(f, app, body, detail),
    }

    let hint =
        "tab alterna abas · ↑/↓ seleciona · a aprova · d nega · r recarrega · Esc/F2 volta ao workbench";
    let text = if app.status.is_empty() {
        hint.to_string()
    } else {
        format!("{} — {hint}", app.status)
    };
    f.render_widget(
        Paragraph::new(text).style(Style::default().fg(Color::DarkGray)),
        footer,
    );
}

/// Aba "ferramentas": o que o orquestrador rodou sozinho, com resultado.
fn draw_tools(f: &mut Frame, app: &mut App, body: Rect, detail: Rect) {
    let items: Vec<ListItem> = app
        .tool_calls
        .iter()
        .map(|(tool, args, _, ok, quando)| {
            let (marca, cor) = if *ok { ("✔", Color::Green) } else { ("✖", Color::Red) };
            ListItem::new(Line::from(vec![
                Span::styled(format!("{marca} "), Style::default().fg(cor)),
                Span::styled(format!("{tool:<16}"), Style::default().fg(Color::Cyan)),
                Span::styled(
                    format!(" {} ", &quando[11..19.min(quando.len())]),
                    Style::default().fg(Color::DarkGray),
                ),
                Span::raw(args.chars().take(60).collect::<String>()),
            ]))
        })
        .collect();
    let vazio = items.is_empty();
    render_list(
        f,
        body,
        items,
        &mut app.selected,
        " ferramentas executadas pelo orquestrador ",
    );
    let texto = if vazio {
        "O orquestrador ainda não executou nenhuma ferramenta de sandbox neste          projeto.

Quando ele testar algo (ui_open, ui_click, ui_exec...), cada          chamada aparece aqui com o resultado — é assim que dá para conferir que          ele realmente testou, em vez de acreditar no relato."
            .to_string()
    } else {
        let i = app.selected.selected().unwrap_or(0).min(app.tool_calls.len() - 1);
        let (tool, args, result, ok, quando) = &app.tool_calls[i];
        format!(
            "{tool} — {}
{quando}

argumentos:
{args}

resultado:
{result}",
            if *ok { "ok" } else { "FALHOU" }
        )
    };
    f.render_widget(detail_paragraph(texto, " detalhe "), detail);
}

fn list_block(title: &str) -> Block<'_> {
    Block::default().borders(Borders::ALL).title(format!(" {title} "))
}

fn render_list(f: &mut Frame, area: Rect, items: Vec<ListItem>, state: &mut ListState, title: &str) {
    let list = List::new(items)
        .block(list_block(title))
        .highlight_style(Style::default().bg(Color::DarkGray).add_modifier(Modifier::BOLD))
        .highlight_symbol("▶ ");
    f.render_stateful_widget(list, area, state);
}

fn detail_paragraph<'a>(text: String, title: &'a str) -> Paragraph<'a> {
    Paragraph::new(text).wrap(Wrap { trim: false }).block(list_block(title))
}

fn draw_decisions(f: &mut Frame, app: &mut App, body: Rect, detail: Rect) {
    let items: Vec<ListItem> = app
        .pending
        .iter()
        .map(|p| {
            // Quem decide vem primeiro: o que é do orquestrador não pede nada.
            let (etiqueta, cor) = if !p.is_for_owner() {
                ("orquestrador decidindo", Color::DarkGray)
            } else if p.is_question() {
                ("pergunta para você", Color::Yellow)
            } else {
                ("aguardando você", Color::Yellow)
            };
            ListItem::new(Line::from(vec![
                Span::styled(
                    format!("[{}] ", p.project),
                    Style::default().fg(Color::Cyan),
                ),
                Span::styled(format!("{etiqueta} · "), Style::default().fg(cor)),
                Span::raw(p.summary.replace('\n', " ")),
            ]))
        })
        .collect();
    let empty = items.is_empty();
    render_list(
        f,
        body,
        items,
        &mut app.selected,
        "decisões — a: aprovar · d: negar · perguntas: /responder no chat",
    );

    let text = if empty {
        "Nada esperando você. No modo autônomo o orquestrador decide o que as CLIs pedem. ✅"
            .to_string()
    } else if let Some(p) = app.selected.selected().and_then(|i| app.pending.get(i)) {
        let opcoes = p
            .options
            .iter()
            .enumerate()
            .map(|(i, o)| format!("  {}) {o}", i + 1))
            .collect::<Vec<_>>()
            .join("\n");
        format!(
            "id: {}\nprojeto: {}\npedido por: {}\nquem decide: {}\ncriada em: {}\n{}\n{}{}",
            p.id,
            p.project,
            if p.requester.is_empty() { "—" } else { &p.requester },
            if p.is_for_owner() { "você" } else { "o orquestrador (modo autônomo)" },
            p.created_at,
            p.note
                .as_deref()
                .map(|n| format!("o orquestrador passou para você: {n}\n"))
                .unwrap_or_default(),
            p.summary,
            if opcoes.is_empty() {
                String::new()
            } else {
                format!(
                    "\n\nalternativas{}:\n{opcoes}",
                    if p.multiple { " (pode marcar várias)" } else { "" }
                )
            }
        )
    } else {
        String::new()
    };
    f.render_widget(detail_paragraph(text, "detalhe"), detail);
}

fn draw_memories(f: &mut Frame, app: &mut App, body: Rect, detail: Rect) {
    let items: Vec<ListItem> = app
        .memories
        .iter()
        .map(|m| {
            let color = match m.kind {
                orchestrator_memory::MemoryKind::Security => Color::Red,
                orchestrator_memory::MemoryKind::Architecture => Color::Blue,
                orchestrator_memory::MemoryKind::Syntax => Color::Green,
                orchestrator_memory::MemoryKind::Decision => Color::Magenta,
                orchestrator_memory::MemoryKind::Practice => Color::Cyan,
            };
            ListItem::new(Line::from(vec![
                Span::styled(format!("[{}] ", m.kind), Style::default().fg(color)),
                Span::raw(m.title.clone()),
                Span::styled(
                    format!("  (prioridade {})", m.priority),
                    Style::default().fg(Color::DarkGray),
                ),
            ]))
        })
        .collect();
    render_list(f, body, items, &mut app.selected, "memórias do projeto");

    let text = app
        .selected
        .selected()
        .and_then(|i| app.memories.get(i))
        .map(|m| format!("{}\n\n{}", m.title, m.body))
        .unwrap_or_else(|| "Sem memórias para este projeto ainda.\nCtrl+Shift+W ou F4 abre o painel de memória (global, projeto, IAs).".into());
    f.render_widget(detail_paragraph(text, "conteúdo"), detail);
}

fn draw_audit(f: &mut Frame, app: &mut App, body: Rect, detail: Rect) {
    let items: Vec<ListItem> = app
        .audit
        .iter()
        .map(|e| {
            let color = match e.decision.as_str() {
                "blocked" | "denied" => Color::Red,
                "approved" => Color::Green,
                _ => Color::White,
            };
            ListItem::new(Line::from(vec![
                Span::styled(format!("[{}] ", e.decision), Style::default().fg(color)),
                Span::raw(e.action.replace('\n', " ")),
            ]))
        })
        .collect();
    render_list(f, body, items, &mut app.selected, "log de auditoria (append-only)");

    let text = app
        .selected
        .selected()
        .and_then(|i| app.audit.get(i))
        .map(|e| {
            format!(
                "sessão: {}\nquando: {}\ndecisão: {}\n\nação: {}\n\nmotivo: {}",
                e.session_id, e.created_at, e.decision, e.action, e.reason
            )
        })
        .unwrap_or_default();
    f.render_widget(detail_paragraph(text, "detalhe"), detail);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wrap_display_short_line_stays_whole() {
        assert_eq!(wrap_display("abc", 10), vec!["abc"]);
        assert_eq!(wrap_display("", 10), vec![""]);
    }

    #[test]
    fn wrap_display_breaks_on_spaces_within_width() {
        assert_eq!(wrap_display("um dois tres", 7), vec!["um dois", "tres"]);
    }

    #[test]
    fn wrap_display_hard_splits_long_words() {
        assert_eq!(wrap_display("abcdefghij", 4), vec!["abcd", "efgh", "ij"]);
    }

    #[test]
    fn wrap_display_counts_display_width_not_bytes() {
        // "águas" tem 5 colunas de exibição, mas 6 bytes UTF-8.
        assert_eq!(wrap_display("águas", 5), vec!["águas"]);
        // Emoji de 2 colunas: três não cabem em 5.
        assert_eq!(wrap_display("💭💭💭", 5), vec!["💭💭", "💭"]);
    }

    #[test]
    fn wrap_display_never_exceeds_width() {
        use unicode_width::UnicodeWidthStr;
        let text = "tarefa: verificar se o hook PreToolUse roda — \
                    incluindo palavras-compridas-demais-para-a-linha e   espaços";
        for w in 1u16..=20 {
            for piece in wrap_display(text, w) {
                assert!(piece.width() <= w as usize, "{piece:?} excede {w} colunas");
            }
        }
    }

    #[test]
    fn hits_respects_rect_bounds_and_empty_rects() {
        let r = Rect { x: 10, y: 5, width: 4, height: 3 };
        assert!(hits(r, (10, 5)));
        assert!(hits(r, (13, 7)));
        // Fora por 1 em cada borda.
        assert!(!hits(r, (14, 7)));
        assert!(!hits(r, (10, 8)));
        assert!(!hits(r, (9, 5)));
        assert!(!hits(r, (10, 4)));
        // Área vazia nunca é atingida (chat escondido).
        assert!(!hits(Rect::default(), (0, 0)));
    }

    #[test]
    fn grid_areas_match_pane_count_and_stay_inside() {
        let area = Rect { x: 0, y: 0, width: 80, height: 20 };
        for n in 1..=8usize {
            let cells = grid_areas(area, n);
            assert_eq!(cells.len(), n, "n={n}");
            for c in cells {
                assert!(c.right() <= area.right() && c.bottom() <= area.bottom());
            }
        }
        assert!(grid_areas(area, 0).is_empty());
    }

    #[test]
    fn add_scroll_saturates_at_zero() {
        assert_eq!(add_scroll(0, -3), 0);
        assert_eq!(add_scroll(2, -3), 0);
        assert_eq!(add_scroll(2, 3), 5);
        assert_eq!(add_scroll(u16::MAX, 3), u16::MAX);
    }

    #[test]
    fn expand_tilde_uses_home_only_at_the_start() {
        std::env::set_var("HOME", "/home/teste");
        assert_eq!(expand_tilde("~/proj"), PathBuf::from("/home/teste/proj"));
        assert_eq!(expand_tilde("~"), PathBuf::from("/home/teste"));
        // Sem `~` inicial fica literal (inclusive `~` no meio).
        assert_eq!(expand_tilde("/abs/x"), PathBuf::from("/abs/x"));
        assert_eq!(expand_tilde("rel/~/x"), PathBuf::from("rel/~/x"));
    }

    #[test]
    fn session_key_is_scoped_by_project_and_workspace() {
        assert_eq!(session_key("default", 0), "chat.session.default.0");
        // Projetos diferentes e workspaces diferentes não compartilham sessão.
        assert_ne!(session_key("a", 0), session_key("b", 0));
        assert_ne!(session_key("a", 0), session_key("a", 1));
    }

    #[test]
    fn chat_width_clamps_to_the_allowed_range() {
        // O layout antigo era 35% fixo (o usuário achou grande demais); hoje
        // o default é menor e ajustável dentro de limites.
        for raw in [0u16, 5, MIN_CHAT_PCT, DEFAULT_CHAT_PCT, 90, u16::MAX] {
            let clamped = raw.clamp(MIN_CHAT_PCT, MAX_CHAT_PCT);
            assert!((MIN_CHAT_PCT..=MAX_CHAT_PCT).contains(&clamped), "raw={raw}");
        }
        assert_eq!(
            DEFAULT_CHAT_PCT.clamp(MIN_CHAT_PCT, MAX_CHAT_PCT),
            DEFAULT_CHAT_PCT
        );
    }
}

/// Testes do fluxo "orquestrador dirige CLI real": a fila `cli_commands`
/// (preenchida pelo MCP em outro processo) é consumida aqui, abre PTYs de
/// verdade e devolve ack. Usa `bash`/`sleep` em vez de `claude` para não
/// depender de binário autenticado.
#[cfg(test)]
mod cli_flow_tests {
    use super::*;

    fn test_app(dir: &std::path::Path) -> App {
        let store = MemoryStore::open_in_memory().unwrap();
        let config = Config::default();
        let mut app = App::new(store, &config, PathBuf::from("/tmp/orchestrator-test.db"));
        // Workspace aponta para um tempdir: o setup do projeto (hooks/MCP)
        // escreve lá, nunca no repositório do teste.
        app.workspaces[0].dir = Some(dir.to_path_buf());
        app
    }

    #[test]
    fn start_command_opens_a_managed_cli_and_acks() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = test_app(tmp.path());
        let project = app.project().to_string();
        let cmd = app
            .store
            .enqueue_cli_command(
                &project,
                "frontend",
                "start",
                &serde_json::json!({ "command": "bash", "args": [], "cwd": "" }).to_string(),
            )
            .unwrap();

        app.poll_cli_commands();

        // Card aberto, gerenciado e focado.
        assert_eq!(app.workspaces[0].panes.len(), 1);
        match &app.workspaces[0].panes[0] {
            Pane::Term(t) => {
                assert_eq!(t.name, "frontend");
                assert!(t.managed, "a CLI do orquestrador deve ser gerenciada");
            }
            _ => panic!("esperava um card de terminal"),
        }
        // Ack devolvido ao MCP com o texto de sucesso.
        let (status, result) = app.store.cli_command_status(&cmd.id).unwrap().unwrap();
        assert_eq!(status, "done");
        assert!(result.unwrap().contains("frontend"));
        // Estado publicado para o `cli_status` do orquestrador.
        let (st, _, _) = app.store.get_cli_state(&project, "frontend").unwrap().unwrap();
        assert_eq!(st, CliState::Starting.label());
    }

    #[test]
    fn orchestrator_clis_get_the_autonomous_env_but_manual_ones_do_not() {
        let tmp = tempfile::tempdir().unwrap();
        let app = test_app(tmp.path());
        // CLI do orquestrador: ninguém digita nela, então o hook precisa ser
        // a autoridade — senão ela trava para sempre num pedido de permissão.
        let managed = app.cli_envs("frontend", true);
        assert!(managed
            .iter()
            .any(|(k, v)| k == "ORCHESTRATOR_AUTONOMOUS" && v == "1"));
        // CLI manual (Ctrl+t): o usuário está lá e responde na tela.
        let manual = app.cli_envs("Claude Code #1", false);
        assert!(!manual.iter().any(|(k, _)| k == "ORCHESTRATOR_AUTONOMOUS"));
        // As duas precisam saber qual banco/projeto usar.
        for envs in [&managed, &manual] {
            assert!(envs.iter().any(|(k, _)| k == "ORCHESTRATOR_DB"));
            assert!(envs.iter().any(|(k, _)| k == "ORCHESTRATOR_PROJECT"));
        }
    }

    #[test]
    fn workspace_remembers_which_clis_it_had() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = test_app(tmp.path());
        app.start_named_cli("frontend", "sh", &[], None).unwrap();
        app.start_named_cli("backend", "sh", &[], None).unwrap();
        app.save_workspace_clis();

        let guardadas = app.saved_workspace_clis();
        assert_eq!(guardadas, vec!["frontend", "backend"]);

        // Cada workspace guarda a sua composição.
        app.switch_workspace(1);
        assert!(app.saved_workspace_clis().is_empty(), "workspace 2 é outra");
        app.switch_workspace(0);
        assert_eq!(app.saved_workspace_clis().len(), 2);
    }

    #[test]
    fn sandbox_gets_the_workspace_folder_by_default() {
        let tmp = tempfile::tempdir().unwrap();
        let app = test_app(tmp.path());
        // `chat_extra_args` publica a pasta que a sandbox vai montar.
        let _ = app.chat_extra_args();
        assert_eq!(
            std::env::var("ORCHESTRATOR_WORKDIR").unwrap(),
            tmp.path().display().to_string(),
            "ui_open sem workdir precisa cair na pasta da workspace"
        );
    }

    #[test]
    fn duplicate_name_fails_with_a_useful_message() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = test_app(tmp.path());
        let project = app.project().to_string();
        let payload = serde_json::json!({ "command": "bash" }).to_string();
        app.store
            .enqueue_cli_command(&project, "frontend", "start", &payload)
            .unwrap();
        app.poll_cli_commands();
        let dup = app
            .store
            .enqueue_cli_command(&project, "frontend", "start", &payload)
            .unwrap();
        app.poll_cli_commands();

        assert_eq!(app.workspaces[0].panes.len(), 1, "não deve duplicar o card");
        let (status, result) = app.store.cli_command_status(&dup.id).unwrap().unwrap();
        assert_eq!(status, "failed");
        assert!(result.unwrap().contains("já existe"));
    }

    #[test]
    fn send_to_unknown_cli_fails_and_explains() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = test_app(tmp.path());
        let project = app.project().to_string();
        let cmd = app
            .store
            .enqueue_cli_command(&project, "fantasma", "send", "faça algo")
            .unwrap();
        app.poll_cli_commands();
        let (status, result) = app.store.cli_command_status(&cmd.id).unwrap().unwrap();
        assert_eq!(status, "failed");
        assert!(result.unwrap().contains("cli_start"));
    }

    #[test]
    fn send_then_stop_drives_and_closes_the_cli() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = test_app(tmp.path());
        let project = app.project().to_string();
        app.store
            .enqueue_cli_command(
                &project,
                "shell",
                "start",
                &serde_json::json!({ "command": "bash" }).to_string(),
            )
            .unwrap();
        app.poll_cli_commands();

        // Prompt entregue: o card entra em "working".
        let send = app
            .store
            .enqueue_cli_command(&project, "shell", "send", "echo oi")
            .unwrap();
        app.poll_cli_commands();
        let (status, result) = app.store.cli_command_status(&send.id).unwrap().unwrap();
        assert_eq!(status, "done");
        assert!(result.unwrap().contains("notificado"));
        match &app.workspaces[0].panes[0] {
            Pane::Term(t) => assert_eq!(t.state(), CliState::Working),
            _ => panic!("esperava terminal"),
        }

        // Stop fecha o card e limpa o estado publicado.
        let stop = app
            .store
            .enqueue_cli_command(&project, "shell", "stop", "")
            .unwrap();
        app.poll_cli_commands();
        assert!(app.workspaces[0].panes.is_empty());
        assert_eq!(
            app.store.cli_command_status(&stop.id).unwrap().unwrap().0,
            "done"
        );
        assert!(app.store.get_cli_state(&project, "shell").unwrap().is_none());
    }

    #[test]
    fn completion_notifies_the_orchestrator_chat_once() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = test_app(tmp.path());
        let project = app.project().to_string();
        app.store
            .enqueue_cli_command(
                &project,
                "worker",
                "start",
                &serde_json::json!({ "command": "bash" }).to_string(),
            )
            .unwrap();
        app.poll_cli_commands();
        app.store
            .enqueue_cli_command(&project, "worker", "send", "echo pronto")
            .unwrap();
        app.poll_cli_commands();

        // Espera a conclusão pela heurística real (quiescência do PTY).
        let mut notified = false;
        for _ in 0..200 {
            app.tick_clis();
            if app.chat.transcript.iter().any(|l| l.who == "sistema") {
                notified = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        assert!(notified, "a conclusão da CLI deveria notificar o chat");
        let line = app
            .chat
            .transcript
            .iter()
            .find(|l| l.who == "sistema")
            .unwrap();
        assert!(line.text.contains("worker"));
        assert!(line.text.contains("cli_status"));
        // Notificação também entra na fila do chat (vira o próximo prompt).
        assert!(app.chat.queued.is_some());

        // Não repete a notificação da mesma conclusão.
        let before = app.chat.transcript.len();
        for _ in 0..5 {
            app.tick_clis();
        }
        assert_eq!(app.chat.transcript.len(), before);
    }

    #[test]
    fn auto_notification_can_be_turned_off() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = test_app(tmp.path());
        app.notify_on_done = false;
        let project = app.project().to_string();
        app.store
            .enqueue_cli_command(
                &project,
                "quieto",
                "start",
                &serde_json::json!({ "command": "bash" }).to_string(),
            )
            .unwrap();
        app.poll_cli_commands();
        app.store
            .enqueue_cli_command(&project, "quieto", "send", "echo x")
            .unwrap();
        app.poll_cli_commands();
        for _ in 0..60 {
            app.tick_clis();
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(
            app.chat.transcript.iter().all(|l| l.who != "sistema"),
            "com /auto off o chat não deve receber notificação"
        );
    }

    #[test]
    fn workspace_dir_is_used_and_validated() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = test_app(tmp.path());
        assert_eq!(app.workspace_dir(), tmp.path());

        // Caminho inexistente é rejeitado com aviso, sem mudar nada.
        app.set_workspace_dir("/nao/existe/mesmo");
        assert!(app.status.contains("não encontrada"));
        assert_eq!(app.workspace_dir(), tmp.path());

        // "-" volta para a pasta do projeto.
        app.set_workspace_dir("-");
        assert_eq!(app.workspace_dir(), app.project_path());
    }

    #[test]
    fn cli_read_returns_history_and_finds_old_lines() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = test_app(tmp.path());
        let project = app.project().to_string();
        app.store
            .enqueue_cli_command(
                &project,
                "shell",
                "start",
                &serde_json::json!({ "command": "bash" }).to_string(),
            )
            .unwrap();
        app.poll_cli_commands();

        // Enche a tela para empurrar a marca para fora dela.
        app.store
            .enqueue_cli_command(
                &project,
                "shell",
                "send",
                "echo MARCA_ANTIGA; for i in $(seq 1 60); do echo enchendo linha $i; done",
            )
            .unwrap();
        app.poll_cli_commands();
        // Espera a CLI terminar (entrega do prompt + execução).
        for _ in 0..120 {
            app.tick_clis();
            if app.chat.transcript.iter().any(|l| l.who == "sistema") {
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
        }

        // A marca já saiu da tela visível…
        let visivel = match &app.workspaces[0].panes[0] {
            Pane::Term(t) => t.screen_text(),
            _ => panic!("esperava terminal"),
        };
        assert!(!visivel.contains("MARCA_ANTIGA"), "a marca deveria ter rolado para fora");

        // …mas cli_read a encontra no histórico.
        let cmd = app
            .store
            .enqueue_cli_command(
                &project,
                "shell",
                "read",
                &serde_json::json!({ "search": "MARCA_ANTIGA", "context": 1 }).to_string(),
            )
            .unwrap();
        app.poll_cli_commands();
        let (status, result) = app.store.cli_command_status(&cmd.id).unwrap().unwrap();
        assert_eq!(status, "done");
        assert!(
            result.unwrap().contains("MARCA_ANTIGA"),
            "cli_read deveria achar o que saiu da tela"
        );
    }

    #[test]
    fn completion_notification_stays_short() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = test_app(tmp.path());
        let project = app.project().to_string();
        app.store
            .enqueue_cli_command(
                &project,
                "worker",
                "start",
                &serde_json::json!({ "command": "bash" }).to_string(),
            )
            .unwrap();
        app.poll_cli_commands();
        app.store
            .enqueue_cli_command(
                &project,
                "worker",
                "send",
                "for i in $(seq 1 40); do echo linha verbosa numero $i; done",
            )
            .unwrap();
        app.poll_cli_commands();
        for _ in 0..120 {
            app.tick_clis();
            if app.chat.transcript.iter().any(|l| l.who == "sistema") {
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        let aviso = app
            .chat
            .transcript
            .iter()
            .find(|l| l.who == "sistema")
            .expect("notificação de conclusão");
        // O que estourava token: a tela inteira ia no prompt a cada conclusão.
        assert!(
            aviso.text.lines().count() <= 2,
            "notificação longa demais:\n{}",
            aviso.text
        );
        assert!(aviso.text.contains("cli_status"));
        assert!(
            !aviso.text.contains("linha verbosa numero 10"),
            "não pode despejar a tela no prompt"
        );
    }

    #[test]
    fn long_messages_are_collapsed_until_the_user_asks() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = test_app(tmp.path());
        let longa = (1..=20)
            .map(|i| format!("linha {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        app.chat.push_line("sistema", longa);
        let entry = app.chat.transcript.last().unwrap();
        assert!(entry.is_long());
        let preview = entry.preview();
        assert!(preview.contains("linha 1"));
        assert!(!preview.contains("linha 20"), "o fim fica escondido");
        assert!(preview.contains("Ctrl+e"));

        // Ctrl+e alterna o modo.
        assert!(!app.expand_chat);
        handle_chat_key(
            &mut app,
            &KeyEvent::new(KeyCode::Char('e'), KeyModifiers::CONTROL),
            true,
            false,
        );
        assert!(app.expand_chat);
    }

    #[test]
    fn chat_queue_is_flushed_by_the_background_drain() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = test_app(tmp.path());
        // Ocupado: a mensagem entra na fila em vez de sumir.
        app.chat.busy = true;
        app.chat.input = "primeira".into();
        let dir = app.workspace_dir();
        app.chat
            .send(String::new(), "default", dir.clone(), None);
        assert!(app.chat.queued.is_some());
        assert!(app.chat.input.is_empty(), "o input não deve ficar preso");
        // Livre: o próximo tick tira da fila (sem provedor real, o envio
        // falha na thread, mas a fila é consumida).
        app.chat.busy = false;
        let taken = app.chat.take_queued();
        assert_eq!(taken.as_deref(), Some("primeira"));
    }
}

/// Testes dos comandos digitados no chat: garantem que o texto que o usuário
/// digita chega às ações certas (parsing + ligação), sem depender de LLM.
#[cfg(test)]
mod chat_command_tests {
    use super::*;

    fn app_in(dir: &std::path::Path) -> App {
        let store = MemoryStore::open_in_memory().unwrap();
        let mut app = App::new(
            store,
            &Config::default(),
            PathBuf::from("/tmp/orchestrator-test.db"),
        );
        app.workspaces[0].dir = Some(dir.to_path_buf());
        app
    }

    /// Digita um comando no chat e aperta Enter.
    fn type_and_enter(app: &mut App, text: &str) {
        app.chat.input = text.to_string();
        handle_chat_key(
            app,
            &KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
            false,
            false,
        );
    }

    #[test]
    fn slash_cli_opens_a_named_cli_card() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = app_in(tmp.path());
        type_and_enter(&mut app, "/cli frontend sh");
        assert_eq!(app.workspaces[0].panes.len(), 1);
        match &app.workspaces[0].panes[0] {
            Pane::Term(t) => assert_eq!(t.name, "frontend"),
            _ => panic!("esperava card de CLI"),
        }
        assert_eq!(app.focus, Focus::Terminal);
        assert!(app.chat.input.is_empty());
    }

    #[test]
    fn slash_cli_without_name_explains_usage() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = app_in(tmp.path());
        type_and_enter(&mut app, "/cli");
        assert!(app.workspaces[0].panes.is_empty());
        assert!(app.status.contains("uso: /cli"));
    }

    #[test]
    fn opening_in_an_unknown_folder_adopts_it_as_a_project() {
        // Simula abrir a CLI numa pasta que não está cadastrada.
        let tmp = tempfile::tempdir().unwrap();
        let pasta = tmp.path().join("loja-nova");
        std::fs::create_dir_all(&pasta).unwrap();
        std::fs::write(
            pasta.join("package.json"),
            r#"{"name":"loja-nova","description":"Catálogo de produtos"}"#,
        )
        .unwrap();

        let store = MemoryStore::open_in_memory().unwrap();
        let mut app = App::new(
            store,
            &Config::default(),
            PathBuf::from("/tmp/orchestrator-test.db"),
        );
        app.config_path = Some(tmp.path().join("config.json"));
        // `adopt_current_dir` usa o cwd; apontamos para a pasta de teste.
        let anterior = std::env::current_dir().unwrap();
        std::env::set_current_dir(&pasta).unwrap();
        let frase = app.adopt_current_dir();
        std::env::set_current_dir(anterior).unwrap();

        let frase = frase.expect("pasta desconhecida deveria ser adotada");
        assert!(frase.contains("loja-nova"), "frase: {frase}");
        assert!(frase.contains("Node"), "deveria reconhecer o ecossistema: {frase}");
        assert!(frase.contains("Catálogo"), "deveria usar a descrição: {frase}");

        // Virou o projeto ativo, com a workspace 1 apontando para a pasta.
        assert_eq!(app.project(), "loja-nova");
        assert_eq!(
            app.workspace_dir().canonicalize().unwrap(),
            pasta.canonicalize().unwrap()
        );
        // E ficou gravado, para a próxima abertura já conhecer.
        let salvo = std::fs::read_to_string(app.config_path.as_ref().unwrap()).unwrap();
        assert!(salvo.contains("loja-nova"));
    }

    #[test]
    fn opening_in_a_known_folder_does_not_duplicate_the_project() {
        let tmp = tempfile::tempdir().unwrap();
        let pasta = tmp.path().join("ja-conhecido");
        std::fs::create_dir_all(&pasta).unwrap();

        let store = MemoryStore::open_in_memory().unwrap();
        let mut config = Config::default();
        config.projects.push(orchestrator_core::ProjectConfig {
            name: "ja-conhecido".into(),
            path: pasta.clone(),
            goal: String::new(),
            cli: "claude_code".into(),
        });
        let mut app = App::new(store, &config, PathBuf::from("/tmp/orchestrator-test.db"));
        app.config = config;

        let anterior = std::env::current_dir().unwrap();
        std::env::set_current_dir(&pasta).unwrap();
        let frase = app.adopt_current_dir();
        std::env::set_current_dir(anterior).unwrap();

        assert!(frase.is_none(), "não deveria adotar de novo");
        assert_eq!(app.projects.len(), 1, "sem duplicata");
        assert_eq!(app.project(), "ja-conhecido");
    }

    #[test]
    fn adoption_avoids_name_collisions() {
        let tmp = tempfile::tempdir().unwrap();
        let store = MemoryStore::open_in_memory().unwrap();
        let mut app = App::new(
            store,
            &Config::default(),
            PathBuf::from("/tmp/orchestrator-test.db"),
        );
        app.projects = vec!["site".into(), "site-2".into()];
        assert_eq!(app.nome_livre("site"), "site-3");
        assert_eq!(app.nome_livre("outro"), "outro");
        assert_eq!(app.nome_livre("  "), "projeto");
        let _ = tmp;
    }

    #[test]
    fn creating_a_project_registers_it_and_switches_to_it() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = app_in(tmp.path());
        let config_path = tmp.path().join("config.json");
        app.config_path = Some(config_path.clone());
        let destino = tmp.path().join("site-novo");

        type_and_enter(
            &mut app,
            &format!("/novo-projeto meu-site {}", destino.display()),
        );

        // A pasta é criada se não existir, o projeto entra na lista e vira o ativo.
        assert!(destino.is_dir(), "a pasta do projeto deveria existir");
        assert!(app.projects.iter().any(|p| p == "meu-site"));
        assert_eq!(app.project(), "meu-site");
        assert_eq!(
            app.project_path().canonicalize().unwrap(),
            destino.canonicalize().unwrap()
        );
        // E fica gravado no config, para existir na próxima abertura.
        let salvo: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&config_path).unwrap()).unwrap();
        assert!(
            salvo["projects"]
                .as_array()
                .unwrap()
                .iter()
                .any(|p| p["name"] == "meu-site"),
            "config: {salvo}"
        );
    }

    #[test]
    fn creating_a_project_refuses_duplicates_and_empty_names() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = app_in(tmp.path());
        app.config_path = Some(tmp.path().join("config.json"));

        type_and_enter(&mut app, "/novo-projeto");
        assert!(app.status.contains("uso:"), "{}", app.status);

        let destino = tmp.path().join("x");
        type_and_enter(&mut app, &format!("/novo-projeto alfa {}", destino.display()));
        let antes = app.projects.len();
        type_and_enter(&mut app, &format!("/novo-projeto alfa {}", destino.display()));
        assert_eq!(app.projects.len(), antes, "não pode duplicar projeto");
        assert!(app.status.contains("já existe"), "{}", app.status);
    }

    #[test]
    fn each_project_keeps_its_own_conversation() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = app_in(tmp.path());
        app.config_path = Some(tmp.path().join("config.json"));
        app.chat.push_line("você", "conversa do projeto original");
        app.persist_chat();

        type_and_enter(
            &mut app,
            &format!("/novo-projeto outro {}", tmp.path().join("outro").display()),
        );
        // Projeto novo começa com a conversa limpa.
        assert!(
            !app.chat.transcript.iter().any(|l| l.text.contains("original")),
            "o projeto novo herdou a conversa: {:?}",
            app.chat.transcript.iter().map(|l| &l.text).collect::<Vec<_>>()
        );
    }

    #[test]
    fn slash_pasta_switches_the_workspace_folder() {
        let tmp = tempfile::tempdir().unwrap();
        let other = tempfile::tempdir().unwrap();
        let mut app = app_in(tmp.path());
        type_and_enter(&mut app, &format!("/pasta {}", other.path().display()));
        assert_eq!(
            app.workspace_dir().canonicalize().unwrap(),
            other.path().canonicalize().unwrap()
        );
        // Persistido para a próxima execução da TUI.
        assert!(app.store.ui_get("ws.0.dir").unwrap().is_some());
    }

    #[test]
    fn each_workspace_has_its_own_chat_session_and_history() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = app_in(tmp.path());
        let project = app.project().to_string();

        // Conversa na workspace 1.
        app.chat.push_line("você", "aqui falo do backend");
        app.chat.session_id = Some("sessao-ws1".into());
        app.persist_chat();

        // Troca para a 2: chat limpo, sessão diferente.
        app.switch_workspace(1);
        assert_eq!(app.ws_idx, 1);
        assert!(
            app.chat.transcript.is_empty(),
            "a workspace 2 não pode herdar a conversa da 1: {:?}",
            app.chat.transcript.iter().map(|l| &l.text).collect::<Vec<_>>()
        );
        assert!(app.chat.session_id.is_none());
        app.chat.push_line("você", "aqui falo do site");
        app.chat.session_id = Some("sessao-ws2".into());
        app.persist_chat();

        // Voltando, a conversa da 1 está intacta.
        app.switch_workspace(0);
        assert!(app
            .chat
            .transcript
            .iter()
            .any(|l| l.text.contains("backend")));
        assert!(!app
            .chat
            .transcript
            .iter()
            .any(|l| l.text.contains("site")));
        assert_eq!(app.chat.session_id.as_deref(), Some("sessao-ws1"));

        // E no banco cada uma tem a sua.
        assert_eq!(
            app.store.ui_get(&session_key(&project, 0)).unwrap().as_deref(),
            Some("sessao-ws1")
        );
        assert_eq!(
            app.store.ui_get(&session_key(&project, 1)).unwrap().as_deref(),
            Some("sessao-ws2")
        );
    }

    #[test]
    fn switching_workspace_keeps_a_running_turn_with_its_own_chat() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = app_in(tmp.path());
        // Turno em andamento na workspace 1.
        app.chat.busy = true;
        app.chat.enqueue("mensagem da workspace 1");
        app.switch_workspace(2);
        // A workspace 3 começa livre, sem herdar fila nem ocupação.
        assert!(!app.chat.busy);
        assert!(app.chat.queued.is_none());
        // Voltando, o turno e a fila continuam lá.
        app.switch_workspace(0);
        assert!(app.chat.busy);
        assert_eq!(
            app.chat.queued.as_deref(),
            Some("mensagem da workspace 1")
        );
    }

    #[test]
    fn slash_nova_clears_session_and_transcript() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = app_in(tmp.path());
        let project = app.project().to_string();
        app.chat.push_line("você", "oi");
        app.chat.session_id = Some("sess-x".into());
        app.persist_chat();
        let ws = app.ws_idx;
        assert!(!app.store.recent_chat_lines(&project, ws, 10).unwrap().is_empty());

        type_and_enter(&mut app, "/nova");
        assert!(app.chat.transcript.is_empty());
        assert!(app.chat.session_id.is_none());
        assert!(app.store.recent_chat_lines(&project, ws, 10).unwrap().is_empty());
        assert!(app.store.ui_get(&session_key(&project, ws)).unwrap().is_none());
    }

    #[test]
    fn slash_auto_toggles_completion_notifications() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = app_in(tmp.path());
        type_and_enter(&mut app, "/auto off");
        assert!(!app.notify_on_done);
        type_and_enter(&mut app, "/auto on");
        assert!(app.notify_on_done);
        type_and_enter(&mut app, "/auto");
        assert!(app.status.contains("ligadas"));
    }

    #[test]
    fn chat_extra_args_block_writing_and_force_the_mcp_config() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = app_in(tmp.path());
        // Sem `.mcp.json` na pasta: nada de --mcp-config (arquivo inexistente).
        let args = app.chat_extra_args();
        assert!(!args.iter().any(|a| a == "--mcp-config"));

        // Com o setup feito, o arquivo existe e passa a ser exigido — sem
        // isso o orquestrador não teria as tools cli_* em modo headless.
        app.ensure_project_setup();
        let args = app.chat_extra_args();
        if app.caps.supports("--mcp-config") {
            let i = args.iter().position(|a| a == "--mcp-config").expect(
                "o .mcp.json do projeto deveria ser passado explicitamente",
            );
            assert!(args[i + 1].ends_with(".mcp.json"));
        }
        if app.caps.supports("--disallowed-tools") {
            let i = args.iter().position(|a| a == "--disallowed-tools").unwrap();
            // O orquestrador delega: não escreve arquivo nem abre subagente.
            for tool in ["Write", "Edit", "Task"] {
                assert!(args[i + 1].contains(tool));
            }
        }
    }

    #[test]
    fn slash_ajuda_opens_the_help_screen() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = app_in(tmp.path());
        type_and_enter(&mut app, "/ajuda");
        assert_eq!(app.screen, Screen::Help);
    }

    #[test]
    fn slash_projeto_reports_unknown_project_without_switching() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = app_in(tmp.path());
        let before = app.project().to_string();
        type_and_enter(&mut app, "/projeto inexistente");
        assert_eq!(app.project(), before);
        assert!(app.status.contains("não existe"));
    }

    #[test]
    fn chat_history_keys_recall_previous_messages() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = app_in(tmp.path());
        app.chat.input_history = vec!["mensagem antiga".into()];
        handle_chat_key(
            &mut app,
            &KeyEvent::new(KeyCode::Up, KeyModifiers::NONE),
            false,
            false,
        );
        assert_eq!(app.chat.input, "mensagem antiga");
        handle_chat_key(
            &mut app,
            &KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
            false,
            false,
        );
        assert!(app.chat.input.is_empty());
    }

    #[test]
    fn layout_keys_resize_and_hide_the_chat() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = app_in(tmp.path());
        let start = app.chat_pct;
        handle_chat_key(
            &mut app,
            &KeyEvent::new(KeyCode::Left, KeyModifiers::CONTROL),
            true,
            false,
        );
        assert!(app.chat_pct < start || start == MIN_CHAT_PCT);
        handle_chat_key(
            &mut app,
            &KeyEvent::new(KeyCode::Char('b'), KeyModifiers::CONTROL),
            true,
            false,
        );
        assert!(app.chat_hidden);
        // Persistido.
        assert_eq!(app.store.ui_get("chat.hidden").unwrap().as_deref(), Some("1"));
    }
}

/// Testes de renderização: desenham as telas num backend de memória. Um
/// pânico no `draw` derruba a TUI inteira do usuário, então vale garantir que
/// cada tela desenha em tamanhos diferentes, com e sem cards.
#[cfg(test)]
mod render_tests {
    use super::*;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    fn app() -> App {
        let store = MemoryStore::open_in_memory().unwrap();
        App::new(
            store,
            &Config::default(),
            PathBuf::from("/tmp/orchestrator-test.db"),
        )
    }

    fn render(app: &mut App, w: u16, h: u16) -> String {
        let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
        term.draw(|f| draw(f, app)).unwrap();
        let buf = term.backend().buffer().clone();
        (0..buf.area.height)
            .map(|y| {
                (0..buf.area.width)
                    .map(|x| buf[(x, y)].symbol().to_string())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn workbench_renders_empty_and_mentions_how_to_start() {
        let mut app = app();
        let out = render(&mut app, 120, 30);
        assert!(out.contains("Orchestrator"));
        assert!(out.contains("Workspace 1"));
        // A tela vazia ensina o caminho novo (orquestrador abre a CLI).
        assert!(out.contains("frontend"), "esperava a dica de abrir CLI:\n{out}");
    }

    #[test]
    fn workbench_renders_with_a_managed_cli_card() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = app();
        app.workspaces[0].dir = Some(tmp.path().to_path_buf());
        app.start_named_cli("frontend", "sh", &[], None).unwrap();
        let out = render(&mut app, 120, 30);
        assert!(out.contains("frontend"));
        // Áreas de clique registradas para o mouse.
        assert_eq!(app.hit.cards.len(), 1);
        assert_eq!(app.hit.ws_tabs.len(), WS_COUNT);
        assert!(app.hit.chat.width > 0);
        assert!(app.hit.divider.width > 0);
    }

    #[test]
    fn hidden_chat_gives_the_whole_width_to_the_grid() {
        let mut app = app();
        app.chat_hidden = true;
        let out = render(&mut app, 100, 24);
        assert!(!out.contains("Orchestrator ["), "o chat deveria estar escondido");
        assert_eq!(app.hit.chat, Rect::default());
        assert_eq!(app.hit.divider, Rect::default());
    }

    #[test]
    fn help_screen_lists_the_new_commands() {
        let mut app = app();
        app.screen = Screen::Help;
        let out = render(&mut app, 100, 40);
        for expected in ["/cli", "/pasta", "/nova", "Alt+b", "F2"] {
            assert!(out.contains(expected), "faltou {expected} na ajuda:\n{out}");
        }
    }

    #[test]
    fn manual_shows_warnings_and_state_in_one_place() {
        let mut app = app();
        app.notify_on_done = false; // gera um aviso conhecido
        app.refresh_notices();
        app.screen = Screen::Help;
        let out = render(&mut app, 110, 44);
        assert!(out.contains("Avisos"), "o manual deveria listar avisos:\n{out}");
        assert!(out.contains("notificações"));
        assert!(out.contains("Estado"));
        assert!(out.contains("projeto"));
    }

    #[test]
    fn manual_says_all_clear_when_there_is_nothing_to_warn() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = app();
        app.workspaces[0].dir = Some(tmp.path().to_path_buf());
        app.ensure_project_setup();
        app.refresh_notices();
        // Um ambiente sem `claude` instalado ainda avisa sobre as flags; o
        // que importa é o formato: ou "nada pendente", ou a seção de avisos.
        app.screen = Screen::Help;
        let out = render(&mut app, 110, 44);
        assert!(out.contains("nada pendente") || out.contains("Avisos"));
    }

    #[test]
    fn footer_is_short_and_points_to_the_manual() {
        let mut app = app();
        app.status = "a".repeat(400);
        app.refresh_notices();
        let out = render(&mut app, 100, 20);
        let footer = out.lines().last().unwrap();
        assert!(footer.contains("Ctrl+h manual"), "rodapé: {footer:?}");
        // Rodapé é UMA linha, sem a antiga parede de atalhos.
        assert!(!footer.contains("Alt+↑/↓"));
        assert!(!footer.contains("/aprovar"));
        assert!(footer.chars().count() <= 100);
    }

    #[test]
    #[ignore]
    fn print_palette() {
        let mut app = app();
        app.chat.input = "/".into();
        println!("=== PALETA (/) ===\n{}", render(&mut app, 110, 26));
        app.chat.input = "/p".into();
        println!("=== FILTRADA (/p) ===\n{}", render(&mut app, 110, 26));
    }

    #[test]
    fn palette_appears_while_typing_a_command() {
        let mut app = app();
        app.chat.input = "/".into();
        let out = render(&mut app, 110, 26);
        assert!(out.contains("/cli"), "a paleta deveria listar comandos:\n{out}");
        assert!(out.contains("/agente"));
        // Marcadores de estado: pronto, falta argumento, indisponível.
        assert!(out.contains('●') && out.contains('○') && out.contains('✖'), "{out}");
    }

    #[test]
    fn palette_in_a_wide_chat_shows_usage_and_reason() {
        let mut app = app();
        app.chat_hidden = false;
        app.set_chat_width(60); // chat largo: cabe uso + descrição + motivo
        app.chat.input = "/aprovar".into();
        let out = render(&mut app, 160, 26);
        assert!(out.contains("Tab completa"), "{out}");
        assert!(out.contains("nenhuma decisão"), "{out}");
    }

    #[test]
    fn palette_filters_and_shows_why_a_command_is_unavailable() {
        let mut app = app();
        app.chat.input = "/aprovar".into();
        let out = render(&mut app, 110, 26);
        // Mesmo estreito, o motivo aparece na linha de detalhe.
        assert!(out.contains("nenhuma decis"), "{out}");
        assert!(out.contains('✖'), "{out}");
    }

    #[test]
    fn palette_says_when_nothing_matches() {
        let mut app = app();
        app.chat.input = "/zzzz".into();
        let out = render(&mut app, 110, 26);
        assert!(out.contains("nenhum comando"), "{out}");
    }

    #[test]
    fn palette_hides_once_the_argument_starts() {
        let mut app = app();
        app.chat.input = "/cli frontend".into();
        let out = render(&mut app, 110, 26);
        assert!(!out.contains("Tab completa"), "paleta deveria sumir:\n{out}");
    }

    #[test]
    fn tools_tab_shows_what_the_orchestrator_ran() {
        let mut app = app();
        let project = app.project().to_string();
        app.store
            .log_tool_call(&project, "ui_open", r#"{"url":"/work/index.html"}"#, "2 elementos", true)
            .unwrap();
        app.store
            .log_tool_call(&project, "ui_click", r#"{"ref":"e9"}"#, "não achei e9", false)
            .unwrap();
        app.reload();
        app.screen = Screen::Manage;
        app.tab = Tab::Tools;
        let out = render(&mut app, 120, 30);
        assert!(out.contains("Ferramentas (2)"), "{out}");
        assert!(out.contains("ui_click"), "{out}");
        assert!(out.contains('✖'), "falha precisa aparecer:\n{out}");
    }

    #[test]
    fn tools_tab_explains_itself_when_empty() {
        let mut app = app();
        app.screen = Screen::Manage;
        app.tab = Tab::Tools;
        let out = render(&mut app, 120, 30);
        assert!(out.contains("ainda não executou"), "{out}");
    }

    #[test]
    fn manage_screen_renders() {
        let mut app = app();
        app.screen = Screen::Manage;
        let out = render(&mut app, 100, 30);
        assert!(!out.trim().is_empty());
    }

    #[test]
    fn renders_at_awkward_sizes_without_panicking() {
        let mut app = app();
        // Terminal minúsculo e estreito: nenhum cálculo de layout pode estourar.
        for (w, h) in [(20u16, 6u16), (40, 10), (200, 60), (16, 4)] {
            let _ = render(&mut app, w, h);
        }
        app.chat_hidden = true;
        let _ = render(&mut app, 20, 6);
        app.screen = Screen::Help;
        let _ = render(&mut app, 20, 6);
    }

    #[test]
    fn chat_shows_the_queue_and_system_notification() {
        let mut app = app();
        app.chat.busy = true;
        app.chat.notify_system("A CLI \"frontend\" concluiu a tarefa.".into());
        let out = render(&mut app, 120, 30);
        assert!(out.contains("sistema"), "notificação deveria aparecer:\n{out}");
        assert!(out.contains("na fila"), "a fila deveria aparecer:\n{out}");
    }
}

/// Testes do preâmbulo automático, dos avisos e da escolha de modelo — as
/// coisas que o usuário pediu depois do primeiro teste ao vivo.
#[cfg(test)]
mod context_and_notices_tests {
    use super::*;
    use orchestrator_memory::defaults;
    use std::time::Instant;
    use orchestrator_memory::MemoryKind;

    fn app_in(dir: &std::path::Path) -> App {
        let store = MemoryStore::open_in_memory().unwrap();
        let mut app = App::new(
            store,
            &Config::default(),
            PathBuf::from("/tmp/orchestrator-test.db"),
        );
        app.workspaces[0].dir = Some(dir.to_path_buf());
        app
    }

    #[test]
    fn preamble_carries_the_workspace_and_leaves_memory_to_the_hook() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = app_in(tmp.path());
        let project = app.project().to_string();
        app.store
            .add_memory(
                &project,
                MemoryKind::Architecture,
                "banco é sqlite",
                "usamos rusqlite com migrações no migrate()",
                3,
            )
            .unwrap();
        app.reload();

        let ctx = app.build_context("como está o banco?");
        assert!(ctx.starts_with("<orchestrator>"));
        assert!(ctx.ends_with("</orchestrator>"));
        assert!(ctx.contains("WORKSPACE:"));
        assert!(ctx.contains(&project));
        assert!(ctx.contains("CLIS ABERTAS: nenhuma"));
        // O índice vem pelo hook UserPromptSubmit: repetir aqui dobraria o
        // custo e criaria duas versões da mesma instrução.
        assert!(!ctx.contains("banco é sqlite"), "índice duplicado:\n{ctx}");
        assert!(!ctx.contains("MEMÓRIAS RELEVANTES"), "{ctx}");
    }

    #[test]
    fn preamble_stays_small_however_much_memory_the_project_has() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = app_in(tmp.path());
        let project = app.project().to_string();
        let corpo = "x".repeat(4000);
        for i in 0..20 {
            app.store
                .add_memory(&project, MemoryKind::Syntax, &format!("nota {i}"), &corpo, 0)
                .unwrap();
        }
        app.reload();
        let ctx = app.build_context("nota");
        assert!(
            ctx.chars().count() < 1500,
            "preâmbulo grande demais ({} chars) — ele vai em TODO turno",
            ctx.chars().count()
        );
    }

    #[test]
    fn each_cli_carries_its_own_name_as_memory_author() {
        let tmp = tempfile::tempdir().unwrap();
        let app = app_in(tmp.path());
        let envs = app.cli_envs("frontend", true);
        assert!(
            envs.iter().any(|(k, v)| k == "ORCHESTRATOR_AGENT" && v == "frontend"),
            "{envs:?}"
        );
    }

    #[test]
    fn f4_opens_the_memory_panel_over_any_focus_and_keeps_keys_from_the_chat() {
        std::env::set_var(
            orchestrator_memory::daemon::SOCKET_ENV,
            std::env::temp_dir().join("orchestrator-tui-teste-sem-memoryd.sock"),
        );
        let tmp = tempfile::tempdir().unwrap();
        let mut app = app_in(tmp.path());
        let f4 = KeyEvent::new(KeyCode::F(4), KeyModifiers::NONE);
        assert!(!handle_key(&mut app, &f4));
        assert!(app.memory_panel.is_some());
        // Com o painel aberto, tecla não vaza para o chat.
        let antes = app.chat.input.clone();
        handle_key(&mut app, &KeyEvent::new(KeyCode::Char('z'), KeyModifiers::NONE));
        assert_eq!(app.chat.input, antes);
        // Ctrl+Shift+W fecha.
        handle_key(
            &mut app,
            &KeyEvent::new(KeyCode::Char('w'), KeyModifiers::CONTROL | KeyModifiers::SHIFT),
        );
        assert!(app.memory_panel.is_none());
    }

    #[test]
    fn preamble_lists_open_clis_so_it_does_not_reopen_them() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = app_in(tmp.path());
        app.start_named_cli("frontend", "sh", &[], None).unwrap();
        let ctx = app.build_context("qualquer coisa");
        assert!(ctx.contains("frontend"), "as CLIs abertas devem aparecer:\n{ctx}");
    }

    #[test]
    fn setup_seeds_security_rules_so_the_gate_has_something_to_raise() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = app_in(tmp.path());
        let project = app.project().to_string();
        assert!(!defaults::has_gate_rules(&app.store, &project).unwrap());

        app.ensure_project_setup();

        assert!(
            defaults::has_gate_rules(&app.store, &project).unwrap(),
            "sem regra, o gate nunca levanta decisão"
        );
        // Idempotente: uma segunda passada não duplica.
        let before = app.store.list(&project, Some(MemoryKind::Security)).unwrap().len();
        app.ensure_project_setup();
        assert_eq!(
            app.store.list(&project, Some(MemoryKind::Security)).unwrap().len(),
            before
        );
    }

    #[test]
    fn notices_are_collected_for_the_manual_not_the_footer() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = app_in(tmp.path());
        // Projeto novo, sem regras: vira aviso.
        app.refresh_notices();
        assert!(
            app.notices.iter().any(|n| n.contains("sem regras")),
            "avisos: {:?}",
            app.notices
        );
        // Depois do setup o aviso some.
        app.ensure_project_setup();
        app.refresh_notices();
        assert!(!app.notices.iter().any(|n| n.contains("sem regras")));

        // Notificação desligada também é um aviso (o orquestrador ficaria cego).
        app.notify_on_done = false;
        app.refresh_notices();
        assert!(app.notices.iter().any(|n| n.contains("notificações")));
    }

    #[test]
    fn model_choice_persists_and_reaches_the_cli_args() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = app_in(tmp.path());
        assert!(app.chat_model.is_empty(), "começa no padrão do provedor");

        app.set_chat_model("opus");
        assert_eq!(app.chat_model, "opus");
        let key = format!("chat.model.{}", app.chat.provider.name);
        assert_eq!(app.store.ui_get(&key).unwrap().as_deref(), Some("opus"));
        if app.caps.supports("--model") {
            let args = app.chat_extra_args();
            let i = args.iter().position(|a| a == "--model").expect("--model");
            assert_eq!(args[i + 1], "opus");
        }

        // Voltar ao padrão limpa a escolha e o argumento.
        app.set_chat_model(DEFAULT_MODEL_LABEL);
        assert!(app.chat_model.is_empty());
        assert!(app.store.ui_get(&key).unwrap().is_none());
        assert!(!app.chat_extra_args().iter().any(|a| a == "--model"));
    }

    #[test]
    fn model_options_always_offer_the_provider_default_first() {
        let tmp = tempfile::tempdir().unwrap();
        let app = app_in(tmp.path());
        let opts = app.model_options();
        assert_eq!(opts[0], DEFAULT_MODEL_LABEL);
    }

    #[test]
    fn slash_modelo_sets_a_free_form_name() {
        let tmp = tempfile::tempdir().unwrap();
        let mut app = app_in(tmp.path());
        app.chat.input = "/modelo claude-opus-5".to_string();
        handle_chat_key(
            &mut app,
            &KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
            false,
            false,
        );
        assert_eq!(app.chat_model, "claude-opus-5");
        // Sem argumento abre o seletor.
        app.chat.input = "/modelo".to_string();
        handle_chat_key(
            &mut app,
            &KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
            false,
            false,
        );
        assert!(matches!(app.picker, Some(Picker::Model(_))));
    }

    #[test]
    fn working_label_reports_elapsed_time_not_thinking() {
        // "pensando" só quando o modelo realmente streamou pensamento.
        assert_eq!(working_label(None), "⚙ trabalhando…");
        let s = working_label(Some(Instant::now()));
        assert!(s.starts_with("⚙ trabalhando…"), "{s}");
        assert!(!s.contains("pensando"));
        let old = Instant::now() - Duration::from_secs(75);
        assert_eq!(working_label(Some(old)), "⚙ trabalhando… 1min15s");
    }

    #[test]
    fn trim_status_keeps_the_footer_on_one_line() {
        assert_eq!(trim_status("curto", 40), "curto");
        let long = "a".repeat(200);
        let cut = trim_status(&long, 30);
        assert_eq!(cut.chars().count(), 30);
        assert!(cut.ends_with('…'));
        // Quebra de linha nunca chega ao rodapé.
        assert_eq!(trim_status("uma\nduas", 40), "uma duas");
    }
}

