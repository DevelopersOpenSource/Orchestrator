//! Painel de memória (Ctrl+Shift+W · F4): onde o dono vê e cura o que as IAs
//! do Orchestrator lembram.
//!
//! Três abas, na ordem de força das memórias:
//! - **Global** — práticas e regras gerais de desenvolvimento do dono, que
//!   valem em todo projeto;
//! - **Projeto** — regras e decisões do dono para o projeto ativo;
//! - **IAs** — o que as IAs do projeto registraram, cada uma com autor.
//!
//! O dono cria e edita as memórias DELE e pode apagar qualquer uma (inclusive
//! uma nota errada de IA). Nota de IA não se edita aqui: editá-la a faria
//! parecer escrita pela IA com as palavras do dono. Toda gravação avisa o
//! `orchestrator-memoryd` para reindexar; se ele não responder, a indexação
//! periódica pega depois.
//!
//! Sobre a tecla: no Konsole, Ctrl+Shift+W é "fechar aba" e só chega à TUI
//! com o protocolo de teclado do kitty ativo (a TUI ativa ao abrir). F4
//! funciona em qualquer terminal.

use std::time::Duration;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use orchestrator_memory::contract::PINNED_PRIORITY;
use orchestrator_memory::daemon::{self, Request};
use orchestrator_memory::store::{MemoryStore, NewMemory};
use orchestrator_memory::{Memory, MemoryKind, Origin, Scope, GLOBAL_PROJECT};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Tabs, Wrap};
use ratatui::Frame;

/// Prazo da busca semântica pelo painel — ela roda na thread da interface.
const SEARCH_TIMEOUT: Duration = Duration::from_secs(3);

/// A tecla abre/fecha o painel?
///
/// Ctrl+Shift+W chega como `W` maiúsculo ou como `w` + SHIFT, conforme o
/// terminal. Um Ctrl+W puro (sem o protocolo, o terminal não distingue) NÃO
/// abre: ele pertence à CLI do card (apagar palavra).
pub fn is_toggle_key(key: &KeyEvent) -> bool {
    if key.code == KeyCode::F(4) {
        return true;
    }
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let shift = key.modifiers.contains(KeyModifiers::SHIFT);
    ctrl && match key.code {
        KeyCode::Char('W') => true,
        KeyCode::Char('w') => shift,
        _ => false,
    }
}

/// As abas do painel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Aba {
    Global,
    Projeto,
    Ias,
}

impl Aba {
    fn indice(self) -> usize {
        match self {
            Aba::Global => 0,
            Aba::Projeto => 1,
            Aba::Ias => 2,
        }
    }

    fn proxima(self) -> Aba {
        match self {
            Aba::Global => Aba::Projeto,
            Aba::Projeto => Aba::Ias,
            Aba::Ias => Aba::Global,
        }
    }

    fn anterior(self) -> Aba {
        match self {
            Aba::Global => Aba::Ias,
            Aba::Projeto => Aba::Global,
            Aba::Ias => Aba::Projeto,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Campo {
    Tipo,
    Titulo,
    Prioridade,
    Corpo,
}

impl Campo {
    fn proximo(self) -> Campo {
        match self {
            Campo::Tipo => Campo::Titulo,
            Campo::Titulo => Campo::Prioridade,
            Campo::Prioridade => Campo::Corpo,
            Campo::Corpo => Campo::Tipo,
        }
    }

    fn anterior(self) -> Campo {
        match self {
            Campo::Tipo => Campo::Corpo,
            Campo::Titulo => Campo::Tipo,
            Campo::Prioridade => Campo::Titulo,
            Campo::Corpo => Campo::Prioridade,
        }
    }
}

#[derive(Debug, Clone)]
struct Formulario {
    /// Id da memória em edição (None = nova).
    editando: Option<String>,
    global: bool,
    kind: MemoryKind,
    titulo: String,
    corpo: String,
    prioridade: String,
    campo: Campo,
}

impl Formulario {
    fn novo(global: bool) -> Self {
        Self {
            editando: None,
            global,
            kind: MemoryKind::Practice,
            titulo: String::new(),
            corpo: String::new(),
            prioridade: "0".into(),
            campo: Campo::Titulo,
        }
    }

    fn de(m: &Memory) -> Self {
        Self {
            editando: Some(m.id.clone()),
            global: m.scope == Scope::Global,
            kind: m.kind,
            titulo: m.title.clone(),
            corpo: m.body.clone(),
            prioridade: m.priority.to_string(),
            campo: Campo::Titulo,
        }
    }
}

#[derive(Debug, Clone)]
enum Modo {
    Navegar,
    Editar(Formulario),
    Buscar(String),
    ConfirmarApagar { id: String, titulo: String },
}

/// O painel aberto.
pub struct MemoryPanel {
    pub aba: Aba,
    itens: Vec<Memory>,
    lista: ListState,
    modo: Modo,
    /// Busca em exibição, quando houver.
    busca: Option<String>,
    /// Última mensagem para o dono (gravou, recusou, apagou…).
    pub mensagem: String,
}

impl MemoryPanel {
    /// Abre na aba do projeto.
    pub fn open(store: &MemoryStore, project: &str) -> Self {
        let mut painel = Self {
            aba: Aba::Projeto,
            itens: Vec::new(),
            lista: ListState::default(),
            modo: Modo::Navegar,
            busca: None,
            mensagem: String::new(),
        };
        painel.recarregar(store, project);
        painel
    }

    /// Relê a aba atual do banco (e sai de uma busca em exibição).
    pub fn recarregar(&mut self, store: &MemoryStore, project: &str) {
        self.busca = None;
        self.itens = match self.aba {
            Aba::Global => store.list(GLOBAL_PROJECT, None).unwrap_or_default(),
            Aba::Projeto => store
                .list(project, None)
                .unwrap_or_default()
                .into_iter()
                .filter(|m| m.origin == Origin::User)
                .collect(),
            Aba::Ias => store
                .list(project, None)
                .unwrap_or_default()
                .into_iter()
                .filter(|m| m.origin == Origin::Agent)
                .collect(),
        };
        self.ajustar_selecao();
    }

    fn ajustar_selecao(&mut self) {
        if self.itens.is_empty() {
            self.lista.select(None);
        } else {
            let i = self.lista.selected().unwrap_or(0).min(self.itens.len() - 1);
            self.lista.select(Some(i));
        }
    }

    fn selecionada(&self) -> Option<&Memory> {
        self.lista.selected().and_then(|i| self.itens.get(i))
    }

    /// Trata uma tecla. Devolve `true` quando o painel deve fechar.
    pub fn handle_key(&mut self, key: &KeyEvent, store: &MemoryStore, project: &str) -> bool {
        match self.modo.clone() {
            Modo::Navegar => return self.navegar(key, store, project),
            Modo::Editar(form) => self.editar(key, form, store, project),
            Modo::Buscar(q) => self.buscar(key, q, store, project),
            Modo::ConfirmarApagar { id, titulo } => self.confirmar(key, &id, &titulo, store, project),
        }
        false
    }

    fn navegar(&mut self, key: &KeyEvent, store: &MemoryStore, project: &str) -> bool {
        match key.code {
            KeyCode::Esc => {
                // Esc primeiro sai da busca; só depois fecha o painel.
                if self.busca.is_some() {
                    self.recarregar(store, project);
                    return false;
                }
                return true;
            }
            KeyCode::Tab | KeyCode::Right => {
                self.aba = self.aba.proxima();
                self.recarregar(store, project);
            }
            KeyCode::BackTab | KeyCode::Left => {
                self.aba = self.aba.anterior();
                self.recarregar(store, project);
            }
            KeyCode::Up => {
                if let Some(i) = self.lista.selected() {
                    self.lista.select(Some(i.saturating_sub(1)));
                }
            }
            KeyCode::Down => {
                if let Some(i) = self.lista.selected() {
                    self.lista.select(Some((i + 1).min(self.itens.len().saturating_sub(1))));
                }
            }
            KeyCode::Char('n') => {
                self.modo = Modo::Editar(Formulario::novo(self.aba == Aba::Global));
                self.mensagem.clear();
            }
            KeyCode::Char('e') => match self.selecionada() {
                Some(m) if m.origin == Origin::User => {
                    self.modo = Modo::Editar(Formulario::de(m));
                    self.mensagem.clear();
                }
                Some(_) => {
                    self.mensagem =
                        "nota de IA não se edita aqui — se estiver errada, apague (d)".into();
                }
                None => {}
            },
            KeyCode::Char('d') => {
                if let Some(m) = self.selecionada() {
                    self.modo = Modo::ConfirmarApagar {
                        id: m.id.clone(),
                        titulo: m.title.clone(),
                    };
                }
            }
            KeyCode::Char('/') => self.modo = Modo::Buscar(String::new()),
            KeyCode::Char('r') => {
                self.recarregar(store, project);
                self.mensagem = "recarregado".into();
            }
            _ => {}
        }
        false
    }

    fn editar(&mut self, key: &KeyEvent, mut form: Formulario, store: &MemoryStore, project: &str) {
        let livre = !key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT);
        match (key.code, form.campo) {
            (KeyCode::Esc, _) => {
                self.modo = Modo::Navegar;
                self.mensagem = "edição cancelada".into();
                return;
            }
            (KeyCode::Tab, c) => form.campo = c.proximo(),
            (KeyCode::BackTab, c) => form.campo = c.anterior(),
            (KeyCode::Left, Campo::Tipo) => form.kind = girar_tipo(form.kind, false),
            (KeyCode::Right, Campo::Tipo) | (KeyCode::Char(' '), Campo::Tipo) => {
                form.kind = girar_tipo(form.kind, true)
            }
            // No corpo, Enter quebra linha (regra de segurança precisa de
            // uma linha `deny-regex:` própria). Nos outros campos, salva.
            (KeyCode::Enter, Campo::Corpo) => form.corpo.push('\n'),
            (KeyCode::Enter, _) => match self.salvar(&form, store, project) {
                Ok(msg) => {
                    if form.editando.is_none() {
                        self.aba = if form.global { Aba::Global } else { Aba::Projeto };
                    }
                    self.modo = Modo::Navegar;
                    self.recarregar(store, project);
                    self.mensagem = msg;
                    return;
                }
                Err(erro) => self.mensagem = erro,
            },
            (KeyCode::Backspace, Campo::Titulo) => {
                form.titulo.pop();
            }
            (KeyCode::Backspace, Campo::Corpo) => {
                form.corpo.pop();
            }
            (KeyCode::Backspace, Campo::Prioridade) => {
                form.prioridade.pop();
            }
            (KeyCode::Char(c), Campo::Titulo) if livre => form.titulo.push(c),
            (KeyCode::Char(c), Campo::Corpo) if livre => form.corpo.push(c),
            (KeyCode::Char(c), Campo::Prioridade)
                if c.is_ascii_digit() || (c == '-' && form.prioridade.is_empty()) =>
            {
                form.prioridade.push(c)
            }
            _ => {}
        }
        self.modo = Modo::Editar(form);
    }

    fn salvar(&self, form: &Formulario, store: &MemoryStore, project: &str) -> Result<String, String> {
        let titulo = form.titulo.trim();
        let corpo = form.corpo.trim();
        if titulo.is_empty() {
            return Err("o título está vazio".into());
        }
        if corpo.is_empty() {
            return Err("o corpo está vazio (Tab chega nele)".into());
        }
        let prioridade: i64 = form.prioridade.trim().parse().unwrap_or(0);
        let gravada = match &form.editando {
            Some(id) => store.update(id, form.kind, titulo, corpo, prioridade),
            None if form.global => store.add(NewMemory::global(form.kind, titulo, corpo, prioridade)),
            None => store.add(NewMemory::user(project, form.kind, titulo, corpo, prioridade)),
        }
        .map_err(|e| format!("não gravou: {e:#}"))?;
        let _ = daemon::call(
            &Request::Index {
                ids: vec![gravada.id.clone()],
            },
            daemon::HEALTH_TIMEOUT,
        );
        let onde = match gravada.scope {
            Scope::Global => "global — vale em todo projeto".to_string(),
            Scope::Project => format!("projeto \"{}\"", gravada.project),
        };
        let fixa = if prioridade >= PINNED_PRIORITY && form.kind != MemoryKind::Security {
            " · regra FIXA: vai em todo prompt"
        } else {
            ""
        };
        Ok(format!("gravada ({onde}){fixa}"))
    }

    fn buscar(&mut self, key: &KeyEvent, mut q: String, store: &MemoryStore, project: &str) {
        match key.code {
            KeyCode::Esc => {
                self.modo = Modo::Navegar;
                return;
            }
            KeyCode::Enter => {
                self.modo = Modo::Navegar;
                let consulta = q.trim().to_string();
                if consulta.is_empty() {
                    return;
                }
                let (itens, semantica) = pesquisar(store, project, &consulta);
                self.itens = itens;
                self.busca = Some(consulta);
                self.lista.select(None);
                self.ajustar_selecao();
                self.mensagem = format!(
                    "{} resultado(s) — {}",
                    self.itens.len(),
                    if semantica {
                        "busca semântica"
                    } else {
                        "palavras-chave (o memoryd não respondeu)"
                    }
                );
                return;
            }
            KeyCode::Backspace => {
                q.pop();
            }
            KeyCode::Char(c)
                if !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                q.push(c)
            }
            _ => {}
        }
        self.modo = Modo::Buscar(q);
    }

    fn confirmar(&mut self, key: &KeyEvent, id: &str, titulo: &str, store: &MemoryStore, project: &str) {
        self.modo = Modo::Navegar;
        if !matches!(key.code, KeyCode::Char('y') | KeyCode::Char('s')) {
            self.mensagem = "nada apagado".into();
            return;
        }
        match store.delete(id) {
            Ok(()) => {
                let _ = daemon::call(
                    &Request::Remove {
                        ids: vec![id.to_string()],
                    },
                    daemon::HEALTH_TIMEOUT,
                );
                self.mensagem = format!("apagada: {titulo}");
            }
            Err(e) => self.mensagem = format!("não apagou: {e:#}"),
        }
        self.recarregar(store, project);
    }
}

/// Próximo (ou anterior) tipo de memória.
fn girar_tipo(atual: MemoryKind, frente: bool) -> MemoryKind {
    let todos = MemoryKind::ALL;
    let i = todos.iter().position(|k| *k == atual).unwrap_or(0);
    let n = todos.len();
    todos[if frente { (i + 1) % n } else { (i + n - 1) % n }]
}

/// Busca pelo memoryd (semântica); se ele não responder, por palavras-chave.
fn pesquisar(store: &MemoryStore, project: &str, consulta: &str) -> (Vec<Memory>, bool) {
    let pedido = Request::Search {
        project: project.to_string(),
        query: consulta.to_string(),
        top_k: 20,
        kind: None,
    };
    if let Ok(resposta) = daemon::call(&pedido, SEARCH_TIMEOUT) {
        let itens = resposta
            .hits
            .iter()
            .filter_map(|h| store.get(&h.id).ok().flatten())
            .collect();
        return (itens, resposta.semantic);
    }
    let itens = store
        .search(project, consulta, 20, None)
        .map(|v| v.into_iter().map(|r| r.memory).collect())
        .unwrap_or_default();
    (itens, false)
}

fn cor(kind: MemoryKind) -> Color {
    match kind {
        MemoryKind::Security => Color::Red,
        MemoryKind::Architecture => Color::Blue,
        MemoryKind::Practice => Color::Cyan,
        MemoryKind::Syntax => Color::Green,
        MemoryKind::Decision => Color::Magenta,
    }
}

/// Retângulo centralizado com a porcentagem dada da área.
fn centro(area: Rect, pct_w: u32, pct_h: u32) -> Rect {
    let w = (area.width as u32 * pct_w / 100) as u16;
    let h = (area.height as u32 * pct_h / 100) as u16;
    Rect {
        x: area.x + (area.width - w) / 2,
        y: area.y + (area.height - h) / 2,
        width: w,
        height: h,
    }
}

/// Desenha o painel por cima do que estiver na tela.
pub fn draw(f: &mut Frame, panel: &mut MemoryPanel, project: &str) {
    let area = centro(f.area(), 92, 88);
    f.render_widget(Clear, area);
    let bloco = Block::default()
        .borders(Borders::ALL)
        .title(" Memória · Ctrl+Shift+W ou F4 fecha ")
        .border_style(Style::default().fg(Color::Cyan));
    let inner = bloco.inner(area);
    f.render_widget(bloco, area);
    let [topo, meio, rodape] = Layout::vertical([
        Constraint::Length(2),
        Constraint::Min(5),
        Constraint::Length(2),
    ])
    .areas(inner);

    let abas = Tabs::new(vec![
        "Global — todo projeto".to_string(),
        format!("Projeto: {project}"),
        "IAs do projeto".to_string(),
    ])
    .select(panel.aba.indice())
    .highlight_style(Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD));
    f.render_widget(abas, topo);

    let [esquerda, direita] =
        Layout::horizontal([Constraint::Percentage(45), Constraint::Percentage(55)]).areas(meio);

    let titulo_lista = match &panel.busca {
        Some(q) => format!(" busca: {q} (Esc volta) "),
        None => format!(" {} memória(s) ", panel.itens.len()),
    };
    let mostrar_origem = panel.busca.is_some() || panel.aba == Aba::Ias;
    let itens: Vec<ListItem> = panel
        .itens
        .iter()
        .map(|m| {
            ListItem::new(Line::from(vec![
                Span::styled(format!("[{} p{}] ", m.kind, m.priority), Style::default().fg(cor(m.kind))),
                Span::raw(m.title.clone()),
                Span::styled(
                    if mostrar_origem {
                        format!("  ({})", m.origin_label())
                    } else {
                        String::new()
                    },
                    Style::default().fg(Color::DarkGray),
                ),
            ]))
        })
        .collect();
    let lista = List::new(itens)
        .block(Block::default().borders(Borders::ALL).title(titulo_lista))
        .highlight_style(Style::default().bg(Color::DarkGray).add_modifier(Modifier::BOLD))
        .highlight_symbol("▶ ");
    f.render_stateful_widget(lista, esquerda, &mut panel.lista);

    match &panel.modo {
        Modo::Editar(form) => desenhar_formulario(f, direita, form),
        _ => {
            let texto = match panel.selecionada() {
                Some(m) => {
                    let fixa = if m.origin == Origin::User
                        && m.priority >= PINNED_PRIORITY
                        && m.kind != MemoryKind::Security
                    {
                        " · FIXA (vai em todo prompt)"
                    } else {
                        ""
                    };
                    format!(
                        "{}\n{} · {} · prioridade {}{fixa}\n\n{}",
                        m.title,
                        m.origin_label(),
                        m.kind,
                        m.priority,
                        m.body
                    )
                }
                None => match panel.aba {
                    Aba::Global => "Nenhuma memória global.\n\nn cria uma — ela vale em TODO projeto \
                                    (ex.: \"commits em português\"). Prioridade 9 ou mais faz dela \
                                    uma regra FIXA, lembrada em todo prompt."
                        .to_string(),
                    Aba::Projeto => "Nenhuma regra sua neste projeto.\n\nn cria uma.".to_string(),
                    Aba::Ias => "Nenhuma IA registrou nada neste projeto ainda.".to_string(),
                },
            };
            f.render_widget(
                Paragraph::new(texto)
                    .wrap(Wrap { trim: false })
                    .block(Block::default().borders(Borders::ALL).title(" conteúdo ")),
                direita,
            );
        }
    }

    let dica = match &panel.modo {
        Modo::Navegar => "Tab/←→ aba · ↑/↓ escolhe · n nova · e edita · d apaga · / busca · Esc fecha",
        Modo::Editar(_) => "Tab campo · ←/→ tipo · Enter salva (no corpo, quebra linha) · Esc cancela",
        Modo::Buscar(_) => "digite e Enter busca · Esc cancela",
        Modo::ConfirmarApagar { .. } => "apagar? y ou s confirma · outra tecla cancela",
    };
    let segunda = match &panel.modo {
        Modo::Buscar(q) => format!("/ {q}▏"),
        Modo::ConfirmarApagar { titulo, .. } => format!("apagar \"{titulo}\"?"),
        _ => panel.mensagem.clone(),
    };
    f.render_widget(
        Paragraph::new(vec![
            Line::from(Span::styled(dica, Style::default().fg(Color::DarkGray))),
            Line::from(Span::styled(segunda, Style::default().fg(Color::Yellow))),
        ]),
        rodape,
    );
}

fn desenhar_formulario(f: &mut Frame, area: Rect, form: &Formulario) {
    let foco = |c: Campo| {
        if form.campo == c {
            Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::Gray)
        }
    };
    let cursor = |c: Campo| if form.campo == c { "▏" } else { "" };
    let cabecalho = format!(
        "{} · {}",
        if form.editando.is_some() { "editando" } else { "nova memória" },
        if form.global { "GLOBAL (todo projeto)" } else { "este projeto" }
    );
    let mut linhas = vec![
        Line::from(Span::styled(cabecalho, Style::default().add_modifier(Modifier::BOLD))),
        Line::from(""),
        Line::from(vec![
            Span::styled("tipo:       ", foco(Campo::Tipo)),
            Span::raw(format!("◀ {} ▶", form.kind)),
        ]),
        Line::from(vec![
            Span::styled("título:     ", foco(Campo::Titulo)),
            Span::raw(format!("{}{}", form.titulo, cursor(Campo::Titulo))),
        ]),
        Line::from(vec![
            Span::styled("prioridade: ", foco(Campo::Prioridade)),
            Span::raw(format!("{}{}", form.prioridade, cursor(Campo::Prioridade))),
        ]),
        Line::from(Span::styled("corpo:", foco(Campo::Corpo))),
    ];
    let partes: Vec<&str> = form.corpo.split('\n').collect();
    for (i, parte) in partes.iter().enumerate() {
        let marca = if i + 1 == partes.len() { cursor(Campo::Corpo) } else { "" };
        linhas.push(Line::from(format!("  {parte}{marca}")));
    }
    linhas.push(Line::from(""));
    if form.kind == MemoryKind::Security {
        linhas.push(Line::from(Span::styled(
            "segurança: uma linha `deny-regex: …` bloqueia; `ask-regex: …` pergunta ao dono",
            Style::default().fg(Color::Red),
        )));
    }
    if form.prioridade.trim().parse::<i64>().unwrap_or(0) >= PINNED_PRIORITY
        && form.kind != MemoryKind::Security
    {
        linhas.push(Line::from(Span::styled(
            "prioridade ≥ 9: regra FIXA, lembrada em todo prompt",
            Style::default().fg(Color::Cyan),
        )));
    }
    f.render_widget(
        Paragraph::new(linhas)
            .wrap(Wrap { trim: false })
            .block(Block::default().borders(Borders::ALL).title(" editar ")),
        area,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tecla(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn digitar(p: &mut MemoryPanel, texto: &str, s: &MemoryStore) {
        for c in texto.chars() {
            p.handle_key(&tecla(KeyCode::Char(c)), s, "loja");
        }
    }

    fn sem_memoryd() {
        // Nunca fala com um memoryd de verdade que rode nesta máquina.
        std::env::set_var(
            daemon::SOCKET_ENV,
            std::env::temp_dir().join("orchestrator-tui-teste-sem-memoryd.sock"),
        );
    }

    #[test]
    fn f4_and_ctrl_shift_w_toggle_but_plain_ctrl_w_belongs_to_the_cli() {
        assert!(is_toggle_key(&tecla(KeyCode::F(4))));
        assert!(is_toggle_key(&KeyEvent::new(
            KeyCode::Char('w'),
            KeyModifiers::CONTROL | KeyModifiers::SHIFT
        )));
        assert!(is_toggle_key(&KeyEvent::new(KeyCode::Char('W'), KeyModifiers::CONTROL)));
        // Ctrl+W puro é "apagar palavra" na CLI do card.
        assert!(!is_toggle_key(&KeyEvent::new(KeyCode::Char('w'), KeyModifiers::CONTROL)));
        assert!(!is_toggle_key(&KeyEvent::new(KeyCode::Char('w'), KeyModifiers::ALT)));
        assert!(!is_toggle_key(&tecla(KeyCode::Char('w'))));
    }

    #[test]
    fn the_owner_creates_a_global_rule_from_the_panel() {
        sem_memoryd();
        let s = MemoryStore::open_in_memory().unwrap();
        let mut p = MemoryPanel::open(&s, "loja");
        assert_eq!(p.aba, Aba::Projeto);
        p.handle_key(&tecla(KeyCode::Left), &s, "loja");
        assert_eq!(p.aba, Aba::Global);

        p.handle_key(&tecla(KeyCode::Char('n')), &s, "loja");
        digitar(&mut p, "commits em português", &s);
        p.handle_key(&tecla(KeyCode::Tab), &s, "loja"); // prioridade
        p.handle_key(&tecla(KeyCode::Backspace), &s, "loja");
        digitar(&mut p, "9", &s);
        p.handle_key(&tecla(KeyCode::Tab), &s, "loja"); // corpo
        digitar(&mut p, "sempre no imperativo", &s);
        p.handle_key(&tecla(KeyCode::Enter), &s, "loja"); // no corpo: quebra linha
        digitar(&mut p, "e curtos", &s);
        p.handle_key(&tecla(KeyCode::Tab), &s, "loja"); // tipo
        p.handle_key(&tecla(KeyCode::Enter), &s, "loja"); // salva

        let globais = s.list(GLOBAL_PROJECT, None).unwrap();
        assert_eq!(globais.len(), 1, "mensagem: {}", p.mensagem);
        let m = &globais[0];
        assert_eq!(m.title, "commits em português");
        assert_eq!(m.body, "sempre no imperativo\ne curtos");
        assert_eq!((m.priority, m.scope, m.origin), (9, Scope::Global, Origin::User));
        assert!(p.mensagem.contains("FIXA"), "{}", p.mensagem);
        assert_eq!(p.itens.len(), 1, "a lista mostra a nova memória");
    }

    #[test]
    fn owner_and_ai_notes_live_in_separate_tabs_and_ai_notes_are_not_edited() {
        sem_memoryd();
        let s = MemoryStore::open_in_memory().unwrap();
        s.add_memory("loja", MemoryKind::Architecture, "postgres", "sqlx", 3).unwrap();
        s.add(NewMemory::agent("loja", "frontend", MemoryKind::Decision, "vite", "rápido", 1))
            .unwrap();
        let mut p = MemoryPanel::open(&s, "loja");
        let titulos = |p: &MemoryPanel| p.itens.iter().map(|m| m.title.clone()).collect::<Vec<_>>();
        assert_eq!(titulos(&p), ["postgres"]);

        p.handle_key(&tecla(KeyCode::Right), &s, "loja");
        assert_eq!(p.aba, Aba::Ias);
        assert_eq!(titulos(&p), ["vite"]);

        // Editar nota de IA: recusa explicando.
        p.handle_key(&tecla(KeyCode::Char('e')), &s, "loja");
        assert!(matches!(p.modo, Modo::Navegar));
        assert!(p.mensagem.contains("apague"), "{}", p.mensagem);

        // Apagar pede confirmação; outra tecla cancela, `y` apaga.
        p.handle_key(&tecla(KeyCode::Char('d')), &s, "loja");
        p.handle_key(&tecla(KeyCode::Char('x')), &s, "loja");
        assert_eq!(s.list("loja", None).unwrap().len(), 2);
        p.handle_key(&tecla(KeyCode::Char('d')), &s, "loja");
        p.handle_key(&tecla(KeyCode::Char('y')), &s, "loja");
        assert_eq!(s.list("loja", None).unwrap().len(), 1);
        assert!(p.itens.is_empty());
    }

    #[test]
    fn editing_an_owner_memory_marks_it_for_reindexing() {
        sem_memoryd();
        let s = MemoryStore::open_in_memory().unwrap();
        let m = s.add_memory("loja", MemoryKind::Architecture, "banco", "sqlx", 3).unwrap();
        s.mark_indexed(std::slice::from_ref(&m.id), "e5").unwrap();
        let mut p = MemoryPanel::open(&s, "loja");
        p.handle_key(&tecla(KeyCode::Char('e')), &s, "loja");
        for _ in 0.."banco".len() {
            p.handle_key(&tecla(KeyCode::Backspace), &s, "loja");
        }
        digitar(&mut p, "banco postgres", &s);
        p.handle_key(&tecla(KeyCode::Enter), &s, "loja");
        assert_eq!(s.get(&m.id).unwrap().unwrap().title, "banco postgres");
        assert_eq!(s.pending_index("e5").unwrap().len(), 1);
    }

    #[test]
    fn an_empty_title_is_refused_with_a_message() {
        sem_memoryd();
        let s = MemoryStore::open_in_memory().unwrap();
        let mut p = MemoryPanel::open(&s, "loja");
        p.handle_key(&tecla(KeyCode::Char('n')), &s, "loja");
        p.handle_key(&tecla(KeyCode::Enter), &s, "loja");
        assert!(matches!(p.modo, Modo::Editar(_)), "continua editando");
        assert!(p.mensagem.contains("título"), "{}", p.mensagem);
        assert!(s.list("loja", None).unwrap().is_empty());
    }

    #[test]
    fn esc_leaves_a_search_before_closing_the_panel() {
        sem_memoryd();
        let s = MemoryStore::open_in_memory().unwrap();
        s.add_memory("loja", MemoryKind::Architecture, "deploy no fly", "flyctl", 3).unwrap();
        let mut p = MemoryPanel::open(&s, "loja");
        p.handle_key(&tecla(KeyCode::Char('/')), &s, "loja");
        digitar(&mut p, "deploy", &s);
        assert!(!p.handle_key(&tecla(KeyCode::Enter), &s, "loja"));
        assert_eq!(p.busca.as_deref(), Some("deploy"));
        assert!(p.mensagem.contains("palavras-chave"), "{}", p.mensagem);
        assert_eq!(p.itens.len(), 1);
        assert!(!p.handle_key(&tecla(KeyCode::Esc), &s, "loja"), "o primeiro Esc sai da busca");
        assert!(p.busca.is_none());
        assert!(p.handle_key(&tecla(KeyCode::Esc), &s, "loja"), "o segundo Esc fecha");
    }
}
