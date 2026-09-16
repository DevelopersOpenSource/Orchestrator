//! Terminais PTY embutidos na TUI — uso manual do usuário E CLIs dirigidas
//! pelo orquestrador.
//!
//! Cada [`TermSession`] roda uma CLI real (Claude Code, Gemini, shell...)
//! dentro de um pseudo-terminal (`portable-pty`, cross-platform: Unix PTY /
//! Windows ConPTY), com a tela emulada por `vt100` e renderizada via
//! `tui-term`. As teclas digitadas pelo usuário com o card focado são
//! encaminhadas ao PTY.
//!
//! NOTA DE ARQUITETURA (decisão explícita do usuário em 2026-09-02): além do
//! uso manual, o ORQUESTRADOR agora dirige CLIs reais por aqui — ele abre uma
//! CLI nomeada (ex.: "frontend"), escreve o prompt no stdin do PTY
//! ([`TermSession::send_prompt`]) e detecta o fim do turno por quiescência da
//! saída + padrão de prompt na tela ([`TermSession::tick`]). Isso substitui a
//! regra anterior ("PTY é só para digitação manual"): o ganho é o usuário ver
//! na tela o comando bash exato, o arquivo escrito e o diff, como numa CLI
//! normal. A contrapartida é que a detecção de conclusão é heurística — os
//! parâmetros ficam nas constantes abaixo.

use std::io::{Read, Write};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use portable_pty::{native_pty_system, ChildKiller, CommandBuilder, MasterPty, PtySize};

/// Tempo mínimo depois do submit antes de aceitar "quieto = terminou" — a
/// CLI leva um momento para começar a produzir saída.
const SPINUP_MIN: Duration = Duration::from_millis(2_000);

/// Quiescência da saída que, junto com um padrão de prompt ocioso na tela,
/// significa "turno concluído".
const QUIET: Duration = Duration::from_millis(2_500);

/// Quiescência longa que conclui o turno MESMO sem casar padrão de prompt.
/// Rede de segurança: sem isso, uma CLI com prompt desconhecido ficaria
/// eternamente em `Working` e o orquestrador nunca seria notificado.
///
/// Só vale quando a CLI não tem processo filho vivo na sessão dela agora
/// ([`TermSession::has_active_children`]) — um shell rodando `npm install`
/// ou um subagente que não imprime nada por um tempo ainda estão
/// trabalhando, e um "concluído" falso aqui deixa o orquestrador CEGO pro
/// resto da tarefa: o estado já virou `Idle`, então `tick` não notifica de
/// novo quando o trabalho de verdade terminar.
const QUIET_HARD: Duration = Duration::from_millis(8_000);

/// Teto absoluto de quiescência: conclui o turno mesmo com filho vivo na
/// sessão. Rede de segurança para um processo órfão que nunca sai (um
/// servidor de teste esquecido rodando) — sem isto, `has_active_children`
/// travaria o card em `Working` para sempre.
const QUIET_ABSOLUTE: Duration = Duration::from_secs(120);

/// Quantas linhas da tela vão no snapshot mandado ao orquestrador.
const TAIL_LINES: usize = 40;

/// Teto de linhas guardadas no histórico próprio da sessão.
///
/// Não dá para usar o scrollback do `vt100` para isto: `set_scrollback` além
/// da altura da tela faz `visible_rows()` estourar (`rows_len - offset` em
/// `grid.rs`), derrubando a TUI. Então mantemos nosso próprio log de linhas
/// já sem escapes ANSI, alimentado pela thread leitora.
const HISTORY_MAX_LINES: usize = 2_000;

/// Espera pelo texto aparecer na tela antes de reenviar o prompt.
const DELIVERY_CONFIRM: Duration = Duration::from_millis(2_000);
/// Quantas vezes reescrevemos o prompt antes de desistir.
const DELIVERY_MAX_TRIES: u8 = 4;

/// Deslocamento máximo seguro do scrollback do vt100 (ver nota acima): a
/// própria altura da tela.
fn safe_scrollback(offset: usize, rows: u16) -> usize {
    offset.min(rows as usize)
}

/// Silêncio que indica "a CLI terminou de desenhar a interface e está
/// esperando input". É o sinal PRIMÁRIO de prontidão: o texto do prompt
/// varia demais (prompts com ícone Nerd Font, powerline, etc.).
const READY_QUIET: Duration = Duration::from_millis(400);

/// Teto de espera pela CLI acabar de subir antes de entregar o primeiro
/// prompt à força. Uma CLI de agente leva alguns segundos para desenhar a
/// caixa de input; escrever antes disso PERDE o texto.
const STARTUP_MAX: Duration = Duration::from_secs(20);

/// O que aconteceu com um prompt entregue a uma CLI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromptDelivery {
    /// Escrito no stdin da CLI agora.
    Sent,
    /// A CLI ainda está subindo: guardado e entregue quando ela estiver
    /// pronta (ou no teto de [`STARTUP_MAX`]).
    Deferred,
}

/// Situação de uma CLI gerenciada pelo orquestrador.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CliState {
    /// Processo subiu, nenhum prompt enviado ainda.
    Starting,
    /// Prompt escrito no stdin, esperando ele APARECER na tela antes de
    /// submeter. Sem esta etapa o texto se perde quando a CLI ainda está
    /// desenhando a interface — e o turno nunca acontece.
    Delivering,
    /// Prompt enviado, turno em andamento.
    Working,
    /// Ociosa, pronta para receber prompt.
    Idle,
    /// Processo terminou.
    Exited,
}

impl CliState {
    /// Rótulo curto publicado no banco (`cli_state.status`) e exibido no card.
    pub fn label(self) -> &'static str {
        match self {
            CliState::Starting => "starting",
            CliState::Delivering => "delivering",
            CliState::Working => "working",
            CliState::Idle => "idle",
            CliState::Exited => "exited",
        }
    }
}

/// Uma sessão de terminal embutida.
pub struct TermSession {
    /// Nome exibido no card (ex.: "Claude Code #1" ou "frontend").
    pub name: String,
    /// A CLI foi aberta pelo orquestrador (recebe prompts e é observada)?
    pub managed: bool,
    /// Último prompt que o orquestrador enviou (exibido no card).
    pub last_prompt: String,
    /// Situação atual (só faz sentido quando `managed`).
    state: CliState,
    parser: Arc<Mutex<vt100::Parser>>,
    /// Quem recebe a saída crua do PTY, na ordem (o xterm.js do app).
    taps: Arc<Mutex<Vec<Sender<Vec<u8>>>>>,
    master: Box<dyn MasterPty + Send>,
    writer: Box<dyn Write + Send>,
    killer: Box<dyn ChildKiller + Send + Sync>,
    /// PID do processo da CLI — que é também o PGID do grupo dela.
    ///
    /// O `portable-pty` chama `setsid` no filho antes de trocar de programa,
    /// então ele nasce líder de sessão e de grupo. Guardamos o número porque
    /// é por ele que se alcança o que a CLI iniciou: matar só o filho deixava
    /// neto vivo (um servidor de teste sobreviveu ao fechamento do card).
    pid: Option<u32>,
    /// A thread ceifeira já colheu o filho (`waitpid` voltou)?
    ///
    /// A partir daí o NÚMERO daquele PID pode ser reciclado pelo sistema, e
    /// nada mais pode ser mandado a ele — nem pelo `ChildKiller`, que no
    /// unix do `portable-pty` é um `kill(pid, SIGHUP)` cru, sem conferência.
    colhido: Arc<AtomicBool>,
    /// No instante da colheita, a sessão da CLI já estava sem ninguém vivo?
    ///
    /// Se estava, o número da sessão ficou livre para reciclagem e o `kill`
    /// não procura mais nada por ele. Se ainda havia membro (um neto órfão),
    /// o kernel mantém o número reservado enquanto esse membro existir.
    sessao_extinta: Arc<AtomicBool>,
    /// `kill` já rodou — o `Drop` chama de novo depois de um fechamento
    /// explícito, e a segunda passada não tem nada legítimo a fazer.
    encerrado: bool,
    exited: Arc<AtomicBool>,
    /// Instante do último byte lido do PTY (carimbado pela thread leitora).
    last_activity: Arc<Mutex<Instant>>,
    /// Histórico de linhas já escritas pela CLI (sem ANSI), para o
    /// orquestrador conseguir buscar o que saiu da tela.
    history: Arc<Mutex<std::collections::VecDeque<String>>>,
    /// Quando o prompt atual foi submetido (base do `SPINUP_MIN`).
    sent_at: Option<Instant>,
    /// Prompt guardado enquanto a CLI ainda está subindo.
    pending_prompt: Option<String>,
    /// Trecho do prompt procurado na tela para confirmar a entrega.
    delivery_sig: String,
    /// Tela imediatamente antes de escrever o prompt (a outra evidência de
    /// que ele chegou: a tela mudou).
    screen_before_delivery: String,
    /// Quando o texto foi escrito (base do reenvio).
    delivered_at: Option<Instant>,
    /// Tentativas de entrega já feitas.
    delivery_tries: u8,
    /// Quando o processo foi criado (base do [`STARTUP_MAX`]).
    spawned_at: Instant,
    rows: u16,
    cols: u16,
}

impl TermSession {
    /// Spawna `command args` dentro de um PTY em `cwd`, com variáveis de
    /// ambiente extras.
    ///
    /// A TUI usa `envs` para exportar `ORCHESTRATOR_DB`/`ORCHESTRATOR_PROJECT`
    /// às CLIs que abre: sem elas o hook `PreToolUse` instalado no projeto
    /// abriria outro banco e as regras de segurança/fila de decisões do
    /// usuário não valeriam dentro da CLI.
    pub fn spawn_env(
        name: String,
        command: &str,
        args: &[String],
        cwd: &Path,
        rows: u16,
        cols: u16,
        envs: &[(String, String)],
    ) -> Result<Self> {
        let pty = native_pty_system();
        let pair = pty
            .openpty(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .context("abrindo PTY")?;

        let mut cmd = CommandBuilder::new(command);
        cmd.args(args);
        if cwd.is_dir() {
            cmd.cwd(cwd);
        }
        cmd.env("TERM", "xterm-256color");
        for (k, v) in envs {
            cmd.env(k, v);
        }

        let mut child = pair
            .slave
            .spawn_command(cmd)
            .with_context(|| format!("iniciando `{command}` no PTY"))?;
        let killer = child.clone_killer();
        // Antes de entregar o `child` à thread que colhe o status.
        let pid = child.process_id();
        // O lado slave fica com o filho; liberamos o nosso handle.
        drop(pair.slave);

        let parser = Arc::new(Mutex::new(vt100::Parser::new(rows, cols, 5000)));
        let exited = Arc::new(AtomicBool::new(false));
        let last_activity = Arc::new(Mutex::new(Instant::now()));
        let history = Arc::new(Mutex::new(std::collections::VecDeque::new()));
        let taps: Arc<Mutex<Vec<Sender<Vec<u8>>>>> = Arc::new(Mutex::new(Vec::new()));

        let mut reader = pair.master.try_clone_reader().context("clonando reader do PTY")?;
        {
            let parser = Arc::clone(&parser);
            let exited = Arc::clone(&exited);
            let last_activity = Arc::clone(&last_activity);
            let history = Arc::clone(&history);
            let taps = Arc::clone(&taps);
            let mut pending = String::new();
            std::thread::spawn(move || {
                let mut buf = [0u8; 8192];
                loop {
                    match reader.read(&mut buf) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => {
                            // Tela e assinantes andam sob a MESMA trava: quem
                            // assina recebe a foto do que já foi processado e,
                            // dali em diante, só o que vier — nada some nem
                            // chega duas vezes.
                            if let Ok(mut p) = parser.lock() {
                                p.process(&buf[..n]);
                                if let Ok(mut t) = taps.lock() {
                                    if !t.is_empty() {
                                        t.retain(|tx| tx.send(buf[..n].to_vec()).is_ok());
                                    }
                                }
                            }
                            // Log próprio: acumula até fechar a linha.
                            pending.push_str(&String::from_utf8_lossy(&buf[..n]));
                            while let Some(i) = pending.find(['\n', '\r']) {
                                let line: String = pending.drain(..=i).collect();
                                let clean = strip_ansi(line.trim_end_matches(['\n', '\r']));
                                let clean = clean.trim_end();
                                if !clean.trim().is_empty() {
                                    if let Ok(mut h) = history.lock() {
                                        if h.back().map(String::as_str) != Some(clean) {
                                            h.push_back(clean.to_string());
                                        }
                                        while h.len() > HISTORY_MAX_LINES {
                                            h.pop_front();
                                        }
                                    }
                                }
                            }
                            // Carimbo de atividade: base da detecção de
                            // quiescência (ver `tick`).
                            if let Ok(mut t) = last_activity.lock() {
                                *t = Instant::now();
                            }
                        }
                    }
                }
                exited.store(true, Ordering::SeqCst);
            });
        }
        // Thread dedicada para colher o exit status (evita zumbis) — e a única
        // que sabe, com certeza, que a CLI saiu.
        let colhido = Arc::new(AtomicBool::new(false));
        let sessao_extinta = Arc::new(AtomicBool::new(false));
        {
            let exited = Arc::clone(&exited);
            let colhido = Arc::clone(&colhido);
            let sessao_extinta = Arc::clone(&sessao_extinta);
            std::thread::spawn(move || {
                let _ = child.wait();
                // A foto da sessão sai ANTES de `colhido`, para quem ler
                // `colhido == true` já encontrar a foto no lugar.
                if cfg!(target_os = "linux") {
                    if let Some(sid) = pid {
                        if membros_da_sessao(sid as i32).is_empty() {
                            sessao_extinta.store(true, Ordering::SeqCst);
                        }
                    }
                }
                colhido.store(true, Ordering::SeqCst);
                // O fim do processo é a verdade sobre "a CLI saiu". O EOF do
                // PTY não chega enquanto um neto segurar o terminal, e o card
                // seguia aparecendo vivo (medido).
                exited.store(true, Ordering::SeqCst);
            });
        }

        let writer = pair.master.take_writer().context("obtendo writer do PTY")?;

        Ok(Self {
            name,
            managed: false,
            last_prompt: String::new(),
            state: CliState::Starting,
            parser,
            taps,
            master: pair.master,
            writer,
            killer,
            pid,
            colhido,
            sessao_extinta,
            encerrado: false,
            exited,
            last_activity,
            history,
            sent_at: None,
            pending_prompt: None,
            delivery_sig: String::new(),
            screen_before_delivery: String::new(),
            delivered_at: None,
            delivery_tries: 0,
            spawned_at: Instant::now(),
            rows,
            cols,
        })
    }

    /// Tela emulada atual (segure o guard só durante a renderização).
    pub fn parser(&self) -> MutexGuard<'_, vt100::Parser> {
        self.parser.lock().expect("mutex do parser envenenado")
    }

    /// Assina a saída crua do PTY — é o que o app desktop entrega ao xterm.js.
    ///
    /// Devolve a tela atual já em sequências de terminal (para pintar de
    /// primeira, mesmo que a CLI tenha escrito antes de alguém olhar) e o
    /// canal com cada pedaço que vier depois, na ordem. Assinante que some é
    /// descartado na próxima escrita.
    pub fn subscribe_output(&self) -> (Vec<u8>, Receiver<Vec<u8>>) {
        let (tx, rx) = channel();
        // Mesma ordem de travas da thread leitora: parser, depois assinantes.
        let parser = self.parser.lock().expect("mutex do parser envenenado");
        let foto = parser.screen().state_formatted();
        if let Ok(mut t) = self.taps.lock() {
            t.push(tx);
        }
        drop(parser);
        (foto, rx)
    }

    /// O processo já terminou?
    pub fn is_exited(&self) -> bool {
        self.exited.load(Ordering::SeqCst)
    }

    /// Situação atual da CLI.
    pub fn state(&self) -> CliState {
        if self.is_exited() {
            CliState::Exited
        } else {
            self.state
        }
    }

    /// Redimensiona o PTY e o emulador, se mudou.
    pub fn resize(&mut self, rows: u16, cols: u16) {
        if rows == 0 || cols == 0 || (rows == self.rows && cols == self.cols) {
            return;
        }
        self.rows = rows;
        self.cols = cols;
        let _ = self.master.resize(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        });
        if let Ok(mut p) = self.parser.lock() {
            p.set_size(rows, cols);
        }
    }

    /// Encaminha bytes crus ao PTY (teclado do usuário).
    pub fn write_bytes(&mut self, bytes: &[u8]) {
        let _ = self.writer.write_all(bytes);
        let _ = self.writer.flush();
    }

    /// Escreve um prompt do ORQUESTRADOR no stdin da CLI e marca o turno como
    /// em andamento. O texto vai como *bracketed paste* (a CLI o trata como
    /// colagem, sem interpretar cada caractere) e o Enter que submete sai no
    /// tick seguinte — ver [`ENTER_DELAY`].
    ///
    /// Quebras de linha são colapsadas em espaço: numa CLI sem suporte a
    /// bracketed paste um `\n` viraria submit no meio do prompt.
    pub fn send_prompt(&mut self, prompt: &str) -> PromptDelivery {
        // Quebras de linha viram espaço: numa CLI sem bracketed paste um
        // `\n` submeteria o prompt no meio.
        let clean: String = prompt
            .chars()
            .map(|c| if c == '\r' || c == '\n' { ' ' } else { c })
            .collect();
        let clean = clean.trim().to_string();
        if clean.is_empty() {
            return PromptDelivery::Sent;
        }
        self.last_prompt = clean.clone();
        self.delivery_tries = 0;
        // CLI ainda desenhando a interface: escrever agora perderia o texto.
        // Guarda e entrega no `tick` quando a caixa de input aparecer.
        if !self.is_ready() {
            self.pending_prompt = Some(clean);
            self.state = CliState::Working;
            return PromptDelivery::Deferred;
        }
        self.deliver_prompt(&clean);
        PromptDelivery::Sent
    }

    /// A CLI já terminou de subir e aceita input?
    ///
    /// Critério primário: ela escreveu algo e então PAROU de escrever
    /// ([`READY_QUIET`]) — vale para qualquer prompt, inclusive os
    /// customizados com ícones que `looks_idle` não reconhece. O padrão de
    /// prompt entra como atalho para o caso de a interface ainda animar.
    fn is_ready(&self) -> bool {
        if self.state != CliState::Starting {
            return true;
        }
        let screen = self.screen_text();
        if screen.trim().is_empty() {
            return false;
        }
        self.quiet_for() >= READY_QUIET || looks_idle(&screen)
    }

    /// Escreve o prompt no stdin como bracketed paste; o Enter sai no tick
    /// seguinte (ver [`ENTER_DELAY`]).
    fn deliver_prompt(&mut self, clean: &str) {
        self.screen_before_delivery = normalize_screen(&self.screen_text());
        // Bracketed paste só serve a quem o habilitou (as TUIs mandam
        // `\x1b[?2004h`). Num campo simples — um `input()`, um `read` de
        // shell — os escapes entram como TEXTO e sujam a resposta; o vt100
        // sabe qual é o caso, então perguntamos a ele.
        let payload = if self.bracketed_paste_on() {
            format!("\x1b[200~{clean}\x1b[201~")
        } else {
            clean.to_string()
        };
        self.write_bytes(payload.as_bytes());
        self.delivery_sig = delivery_signature(clean);
        self.delivered_at = Some(Instant::now());
        self.delivery_tries += 1;
        self.state = CliState::Delivering;
        self.touch_activity();
    }

    /// A aplicação dentro do PTY ligou o modo bracketed paste?
    fn bracketed_paste_on(&self) -> bool {
        self.parser().screen().bracketed_paste()
    }

    /// O que escrevemos já chegou à CLI?
    ///
    /// Duas evidências, porque nenhuma sozinha basta: o FIM do prompt visível
    /// na tela (a caixa rola com o cursor), ou simplesmente a tela ter MUDADO
    /// desde antes de escrevermos — CLIs resumem paste grande como
    /// "[Pasted text #1 +40 lines]", e aí o texto nunca aparece literalmente.
    fn delivery_confirmed(&self) -> bool {
        if self.delivery_sig.is_empty() {
            return true;
        }
        let agora = normalize_screen(&self.screen_text());
        agora.contains(&self.delivery_sig) || agora != self.screen_before_delivery
    }

    /// Marca atividade agora (usado ao enviar prompt: o relógio de
    /// quiescência conta a partir do envio, não de uma leitura antiga).
    fn touch_activity(&self) {
        if let Ok(mut t) = self.last_activity.lock() {
            *t = Instant::now();
        }
    }

    /// Quanto tempo o PTY está sem produzir saída.
    fn quiet_for(&self) -> Duration {
        self.last_activity
            .lock()
            .map(|t| t.elapsed())
            .unwrap_or_default()
    }

    /// Texto da tela emulada (linhas com espaços à direita aparados).
    pub fn screen_text(&self) -> String {
        let guard = self.parser();
        let raw = guard.screen().contents();
        raw.lines()
            .map(|l| l.trim_end())
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Últimas linhas não vazias da tela — é o que o orquestrador recebe
    /// como "estado" da CLI (em `cli_status` e na notificação de conclusão).
    pub fn screen_tail(&self, lines: usize) -> String {
        let text = self.screen_text();
        let kept: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
        let start = kept.len().saturating_sub(lines);
        kept[start..].join("\n")
    }

    /// Avança a máquina de estados da CLI gerenciada. Chame a cada tick da
    /// UI. Devolve `Some(tail)` exatamente UMA vez, no instante em que o
    /// turno termina (`Working` → `Idle`) — é o gatilho da notificação
    /// "a CLI X concluiu a tarefa" para o orquestrador.
    pub fn tick(&mut self) -> Option<String> {
        // Prompt guardado enquanto a CLI subia: entrega quando ela mostra a
        // caixa de input, ou no teto de arranque (não deixa preso).
        if self.pending_prompt.is_some() && !self.is_exited() {
            let ready = self.is_ready();
            let timed_out = self.spawned_at.elapsed() >= STARTUP_MAX;
            if ready || timed_out {
                if let Some(prompt) = self.pending_prompt.take() {
                    self.state = CliState::Idle; // libera o guard de is_ready
                    self.deliver_prompt(&prompt);
                }
            }
            return None;
        }
        // Etapa de entrega: só submete quando vê o texto na tela; se ele não
        // aparecer, reescreve (a CLI pode ter engolido durante o arranque).
        if self.state == CliState::Delivering {
            if self.delivery_confirmed() {
                self.write_bytes(b"\r");
                self.state = CliState::Working;
                self.sent_at = Some(Instant::now());
                self.delivery_sig.clear();
                self.delivered_at = None;
                self.touch_activity();
            } else if self
                .delivered_at
                .map(|t| t.elapsed() >= DELIVERY_CONFIRM)
                .unwrap_or(false)
            {
                if self.delivery_tries >= DELIVERY_MAX_TRIES {
                    self.state = CliState::Idle;
                    self.delivery_sig.clear();
                    self.delivered_at = None;
                    return Some(format!(
                        "não consegui entregar o prompt à CLI \"{}\": o texto não \
                         apareceu na tela dela depois de {DELIVERY_MAX_TRIES} tentativas \
                         — veja se ela está esperando alguma confirmação",
                        self.name
                    ));
                }
                // Limpa a caixa antes de reescrever: sem isto as tentativas
                // se acumulam e a CLI receberia o prompt repetido.
                self.write_bytes(&[0x15]); // Ctrl+U
                let prompt = self.last_prompt.clone();
                self.deliver_prompt(&prompt);
            }
            return None;
        }
        if !self.managed {
            return None;
        }
        if self.is_exited() {
            let was_working = self.state == CliState::Working;
            self.state = CliState::Exited;
            // Processo morreu no meio do turno: o orquestrador precisa saber.
            return was_working.then(|| self.screen_tail(TAIL_LINES));
        }
        if self.state != CliState::Working {
            return None;
        }
        // Ainda subindo: não conta como quieto.
        if self.sent_at.map(|t| t.elapsed()) < Some(SPINUP_MIN) {
            return None;
        }
        let quiet = self.quiet_for();
        let screen = self.screen_text();
        let done = (quiet >= QUIET && looks_idle(&screen))
            || (quiet >= QUIET_HARD && !self.has_active_children())
            || quiet >= QUIET_ABSOLUTE;
        if !done {
            return None;
        }
        self.state = CliState::Idle;
        self.sent_at = None;
        Some(self.screen_tail(TAIL_LINES))
    }

    /// A CLI tem algum processo filho vivo na sessão dela agora — um shell
    /// rodando um comando, um subagente que ela abriu?
    ///
    /// `self.pid` é o líder de sessão (o `portable-pty` chama `setsid` antes
    /// de trocar de programa — ver o campo). Mais de um membro na sessão
    /// significa que existe ALGUÉM além do processo raiz da CLI ainda vivo.
    /// Só em Linux (`/proc`); nas outras plataformas volta `false`, e o
    /// comportamento cai no [`QUIET_HARD`] de antes.
    fn has_active_children(&self) -> bool {
        cfg!(target_os = "linux")
            && self
                .pid
                .is_some_and(|sid| membros_da_sessao(sid as i32).len() > 1)
    }

    /// Histórico da CLI (o que ela escreveu desde que abriu), da linha mais
    /// antiga para a mais recente.
    ///
    /// A tela sozinha mostra só as últimas ~24 linhas: sem isto o
    /// orquestrador não consegue "buscar mensagens anteriores" — ele não vê
    /// o que já rolou para fora da tela.
    pub fn history(&self, max_lines: usize) -> Vec<String> {
        let Ok(h) = self.history.lock() else {
            return Vec::new();
        };
        let start = h.len().saturating_sub(max_lines.min(HISTORY_MAX_LINES));
        h.iter().skip(start).cloned().collect()
    }

    /// Procura `needle` no histórico (sem diferenciar maiúsculas) e devolve
    /// as linhas que casaram com `context` linhas ao redor de cada uma.
    pub fn search(&self, needle: &str, context: usize, max_hits: usize) -> Vec<String> {
        let lines = self.history(HISTORY_MAX_LINES);
        let needle = needle.to_lowercase();
        let mut out: Vec<String> = Vec::new();
        let mut hits = 0;
        let mut last_end = 0usize;
        for (i, l) in lines.iter().enumerate() {
            if !l.to_lowercase().contains(&needle) {
                continue;
            }
            hits += 1;
            let from = i.saturating_sub(context);
            let to = (i + context + 1).min(lines.len());
            // Trechos separados ganham uma marca; sobrepostos viram um bloco.
            if from > last_end && !out.is_empty() {
                out.push("…".to_string());
            }
            for l in lines.iter().take(to).skip(from.max(last_end)) {
                out.push(l.clone());
            }
            last_end = to;
            if hits >= max_hits {
                break;
            }
        }
        out
    }

    /// Rola o histórico do terminal (roda do mouse): `delta > 0` sobe.
    pub fn scroll_by(&mut self, delta: i16) {
        let rows = self.rows;
        if let Ok(mut p) = self.parser.lock() {
            let current = p.screen().scrollback() as i32;
            let next = (current + delta as i32).max(0) as usize;
            // Limite obrigatório: passar da altura da tela faz o vt100
            // estourar em `visible_rows()` e derruba a TUI inteira.
            p.set_scrollback(safe_scrollback(next, rows));
        }
    }

    /// Grupos de processos a encerrar junto com esta CLI, lidos AGORA.
    ///
    /// São os grupos de todo processo vivo na SESSÃO da CLI, não só o grupo
    /// do filho: o filho nasce líder de sessão (o `portable-pty` chama
    /// `setsid`), tudo que ele inicia fica na sessão dele, mas não
    /// necessariamente no grupo — com controle de jobs (`set -m`, o normal num
    /// shell interativo) cada job em segundo plano ganha grupo próprio, e
    /// mirar só o grupo do filho o deixava vivo (medido).
    fn grupos(&self) -> Vec<i32> {
        let Some(sid) = self.pid.map(|p| p as i32) else {
            return Vec::new();
        };
        // Sessão que já estava vazia na colheita: o número pode ser de outro.
        if self.sessao_extinta.load(Ordering::SeqCst) {
            return Vec::new();
        }
        // Sem `/proc` não há foto da sessão, então só enquanto o filho vive.
        if !cfg!(target_os = "linux") && self.colhido.load(Ordering::SeqCst) {
            return Vec::new();
        }
        grupos_da_sessao(sid)
    }

    /// Encerra a CLI e tudo que ela iniciou na sessão dela.
    ///
    /// Antes daqui só o filho recebia sinal. Um neto comum morria mesmo assim
    /// (quando o líder da sessão morre, o kernel manda SIGHUP ao grupo em
    /// primeiro plano), mas dois casos ficavam vivos no host — os dois
    /// medidos: o que ignora SIGHUP (servidor subido com `nohup`) e o job em
    /// grupo próprio. Agora vai SIGHUP a cada grupo da sessão, um prazo curto,
    /// e SIGKILL no que ainda estiver vivo.
    ///
    /// Limite medido e fixado em teste: neto que chama `setsid` abre sessão
    /// nova e sobrevive. Alcançá-lo exigiria varrer processos por nome, o que
    /// este projeto não faz.
    pub fn kill(&mut self) {
        if self.encerrado {
            return;
        }
        self.encerrado = true;
        encerra_grupos(self.pid.map(|p| p as i32), &self.grupos());
        // `ChildKiller::kill` no unix do portable-pty é `kill(pid, SIGHUP)` num
        // número cru: não é SIGKILL e não confere nada. Só pode ser usado
        // enquanto o filho não foi colhido — depois disso o número pode ser de
        // outro processo. No unix ele é redundante (a sessão já levou o
        // sinal); no Windows é `TerminateProcess`, o único encerramento que há.
        if !self.colhido.load(Ordering::SeqCst) {
            let _ = self.killer.kill();
        }
    }
}

impl Drop for TermSession {
    fn drop(&mut self) {
        self.kill();
    }
}

/// Quanto esperar entre o pedido de saída e a força.
///
/// Curto de propósito: roda na thread da interface (fechar card com Ctrl+w),
/// e o que se ganha aqui é a CLI conseguir salvar o estado dela e um
/// servidor remover socket/pidfile antes do SIGKILL.
const GRACE_ENCERRAMENTO: Duration = Duration::from_millis(120);

/// Este grupo pode receber sinal?
///
/// A guarda mais importante do arquivo: `killpg(0, …)` manda o sinal para o
/// NOSSO grupo — a TUI se mataria junto, e junto com ela o que a hospeda.
/// Grupo 1 é o init. Fora disso, só o que não é o nosso.
pub fn grupo_sinalizavel(pgid: i32, meu: i32) -> bool {
    pgid > 1 && pgid != meu
}

/// Processos VIVOS de uma sessão, como `(pid, pgid)` — lidos de `/proc`.
///
/// Por NÚMERO de sessão, nunca por nome: varrer processos por padrão de nome
/// já derrubou a TUI, a sessão que a hospedava e o aplicativo do usuário.
/// Zumbi não conta: já morreu, só não foi colhido.
#[cfg(target_os = "linux")]
fn membros_da_sessao(sid: i32) -> Vec<(i32, i32)> {
    let mut achados = Vec::new();
    let Ok(dir) = std::fs::read_dir("/proc") else {
        return achados;
    };
    for entrada in dir.flatten() {
        let Ok(pid) = entrada.file_name().to_string_lossy().parse::<i32>() else {
            continue;
        };
        let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) else {
            continue;
        };
        // O nome do programa vem entre parênteses e pode conter espaço: os
        // campos confiáveis começam depois do ÚLTIMO ')' — estado, ppid,
        // grupo, sessão.
        let Some((_, depois)) = stat.rsplit_once(')') else {
            continue;
        };
        let campos: Vec<&str> = depois.split_whitespace().collect();
        if campos.len() < 4 || campos[0] == "Z" || campos[0] == "X" {
            continue;
        }
        if campos[3].parse() == Ok(sid) {
            if let Ok(pgid) = campos[2].parse() {
                achados.push((pid, pgid));
            }
        }
    }
    achados
}

#[cfg(not(target_os = "linux"))]
fn membros_da_sessao(_sid: i32) -> Vec<(i32, i32)> {
    Vec::new()
}

/// Grupos distintos com processo vivo na sessão.
#[cfg(target_os = "linux")]
fn grupos_da_sessao(sid: i32) -> Vec<i32> {
    let mut grupos: Vec<i32> = membros_da_sessao(sid).into_iter().map(|(_, g)| g).collect();
    grupos.sort_unstable();
    grupos.dedup();
    grupos
}

/// Sem `/proc`: o grupo do próprio filho é o que dá para afirmar.
#[cfg(not(target_os = "linux"))]
fn grupos_da_sessao(sid: i32) -> Vec<i32> {
    vec![sid]
}

/// Pede a saída de cada grupo (SIGHUP) e, passado o prazo, força (SIGKILL).
#[cfg(unix)]
fn encerra_grupos(sid: Option<i32>, grupos: &[i32]) {
    let meu_grupo = unsafe { libc::getpgrp() };
    // A sessão da CLI nunca é a nossa (o filho fez `setsid`). Se o número
    // bater, algo está muito errado — e o erro seguro é não mandar nada.
    if sid.is_some() && sid == Some(unsafe { libc::getsid(0) }) {
        return;
    }
    let alvos: Vec<i32> = grupos
        .iter()
        .copied()
        .filter(|g| grupo_sinalizavel(*g, meu_grupo))
        .collect();
    if alvos.is_empty() {
        return;
    }
    for g in &alvos {
        // SIGHUP é o que o terminal manda quando fecha: quem trata, sai.
        unsafe { libc::killpg(*g, libc::SIGHUP) };
    }
    // A espera termina assim que os grupos esvaziam — o caso comum é a CLI
    // sair no SIGHUP, e aí fechar o card não custa nada. Checagem barata
    // (sinal 0), que conta zumbi como vivo: no pior caso espera o prazo.
    let limite = Instant::now() + GRACE_ENCERRAMENTO;
    while Instant::now() < limite && alvos.iter().copied().any(grupo_existe) {
        std::thread::sleep(Duration::from_millis(10));
    }
    // A força só vai a grupo que AINDA tem processo vivo, conferido agora.
    // Um grupo que esvaziou no prazo não recebe SIGKILL: seu número pode já
    // estar livre. E enquanto há membro vivo o número segue reservado, então
    // entre esta conferência e o `killpg` não há como ele virar de outro.
    for g in grupos_ainda_vivos(sid, &alvos) {
        unsafe { libc::killpg(g, libc::SIGKILL) };
    }
}

/// Dos `alvos`, os grupos que ainda têm processo vivo.
#[cfg(unix)]
fn grupos_ainda_vivos(sid: Option<i32>, alvos: &[i32]) -> Vec<i32> {
    if cfg!(target_os = "linux") {
        if let Some(sid) = sid {
            let vivos = grupos_da_sessao(sid);
            return alvos.iter().copied().filter(|g| vivos.contains(g)).collect();
        }
    }
    alvos.iter().copied().filter(|g| grupo_existe(*g)).collect()
}

/// O grupo ainda tem algum processo (inclusive zumbi)?
#[cfg(unix)]
fn grupo_existe(pgid: i32) -> bool {
    // Sinal 0 não entrega nada: só pergunta se haveria a quem entregar.
    unsafe { libc::killpg(pgid, 0) == 0 }
}

#[cfg(not(unix))]
fn encerra_grupos(_sid: Option<i32>, _grupos: &[i32]) {
    // Sem grupo de processos POSIX: sobra o encerramento do próprio filho.
}

/// Tamanho da assinatura de entrega.
const SIGNATURE_CHARS: usize = 24;

/// Trecho do prompt usado para confirmar que ele chegou à tela.
///
/// É o FINAL do texto, não o começo: a caixa de input rola com o cursor, e
/// num prompt longo o começo sai de vista — procurar por ele daria "não
/// entregue" mesmo com o texto inteiro lá dentro.
pub fn delivery_signature(prompt: &str) -> String {
    let flat = normalize_screen(prompt);
    let n = flat.chars().count();
    flat.chars().skip(n.saturating_sub(SIGNATURE_CHARS)).collect()
}

/// Normaliza para comparar texto de tela: sem espaços repetidos nem quebras.
pub fn normalize_screen(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Remove sequências de escape ANSI de uma linha, deixando só o texto.
///
/// Cobre CSI (`ESC [ … letra`), OSC (`ESC ] … BEL|ESC \\`) e escapes de dois
/// caracteres. É o suficiente para o histórico ficar legível e buscável.
fn strip_ansi(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\u{1b}' {
            if !c.is_control() || c == '\t' {
                out.push(c);
            }
            continue;
        }
        match chars.next() {
            Some('[') => {
                for c in chars.by_ref() {
                    if c.is_ascii_alphabetic() || c == '@' || c == '~' {
                        break;
                    }
                }
            }
            Some(']') => {
                while let Some(c) = chars.next() {
                    if c == '\u{7}' {
                        break;
                    }
                    if c == '\u{1b}' {
                        chars.next();
                        break;
                    }
                }
            }
            _ => {}
        }
    }
    out
}

/// O caractere está numa faixa de uso privado (ícones de Nerd Font)?
fn is_private_use(c: char) -> bool {
    matches!(c as u32, 0xE000..=0xF8FF | 0xF0000..=0xFFFFD | 0x100000..=0x10FFFD)
}

/// A tela parece estar num prompt ocioso, esperando digitação?
///
/// Heurística deliberada (ver nota de arquitetura no topo): procura a caixa
/// de input das CLIs de agente (`│ >`), um prompt de shell (`$`/`#`/`❯` no
/// fim) e descarta a tela quando há um indicador explícito de trabalho em
/// andamento ("esc to interrupt", spinner de token, etc.).
pub fn looks_idle(screen: &str) -> bool {
    let lower = screen.to_lowercase();
    // Sinais fortes de "ainda trabalhando" vencem qualquer prompt na tela.
    for busy in [
        "esc to interrupt",
        "esc para interromper",
        "ctrl+c to stop",
        "running…",
        "running...",
    ] {
        if lower.contains(busy) {
            return false;
        }
    }
    let tail: Vec<&str> = screen
        .lines()
        .map(|l| l.trim_end())
        .filter(|l| !l.trim().is_empty())
        .rev()
        .take(6)
        .collect();
    for line in tail {
        let t = line.trim();
        // Caixa de input das CLIs de agente: "│ > ..." (com ou sem texto).
        if t.starts_with('│') && t.contains('>') {
            return true;
        }
        if t.starts_with("> ") || t == ">" {
            return true;
        }
        // Prompts que ficam no INÍCIO da linha (oh-my-zsh `➜  ~/dir`,
        // starship `❯ `, etc.).
        if let Some(first) = t.chars().next() {
            if matches!(first, '➜' | '❯' | 'λ' | '»' | '▶')
                && t.chars().nth(1).map(char::is_whitespace).unwrap_or(true)
            {
                return true;
            }
        }
        // Prompt de shell — inclusive os customizados: além de `$`/`#`,
        // aceita as setas usuais e qualquer glifo da Private Use Area, que é
        // onde vivem os ícones de Nerd Font usados por powerline/p10k.
        if let Some(last) = t.chars().last() {
            if matches!(last, '$' | '#' | '%' | '❯' | '➜' | '»' | 'λ' | '▶')
                || is_private_use(last)
            {
                return true;
            }
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn looks_idle_detects_agent_input_box() {
        let screen = "algum output\n╭────────────╮\n│ >          │\n╰────────────╯";
        assert!(looks_idle(screen));
    }

    #[test]
    fn looks_idle_detects_shell_prompt() {
        assert!(looks_idle("total 4\nuser@host:~/proj$"));
        assert!(looks_idle("saida\n~/proj ❯"));
        assert!(looks_idle("saida\n➜  proj"));
        assert!(looks_idle("saida\nroot@box:/#"));
    }

    #[test]
    fn looks_idle_detects_prompts_that_end_in_nerd_font_icons() {
        // Prompt real do usuário (powerline/p10k): termina em glifo da
        // Private Use Area, não em `$`. Foi o caso que quebrou a detecção.
        let real = "░▒▓ \u{f380} \u{e0b0} /tmp \u{e0b0} ▓▒░\n··•\u{f0da}";
        assert!(looks_idle(real), "prompt com ícone deveria contar como ocioso");
        assert!(is_private_use('\u{f0da}'));
        assert!(!is_private_use('a'));
    }

    #[test]
    fn looks_idle_rejects_busy_screen() {
        // Indicador explícito de trabalho em andamento vence o prompt na tela.
        let screen = "│ >          │\nesperando... (esc to interrupt)";
        assert!(!looks_idle(screen));
    }

    #[test]
    fn looks_idle_rejects_plain_output() {
        assert!(!looks_idle("compilando crate foo\nerro: algo deu errado"));
    }

    #[test]
    fn cli_state_labels_are_stable() {
        assert_eq!(CliState::Starting.label(), "starting");
        assert_eq!(CliState::Working.label(), "working");
        assert_eq!(CliState::Idle.label(), "idle");
        assert_eq!(CliState::Exited.label(), "exited");
    }
}

#[cfg(test)]
mod pty_tests {
    use super::*;

    /// Abre um PTY real rodando `sh` e espera ele mostrar o prompt.
    fn sh() -> TermSession {
        let mut t = TermSession::spawn_env(
            "teste".into(),
            "sh",
            &[],
            std::path::Path::new("/tmp"),
            24,
            80,
            &[],
        )
        .expect("spawn do sh");
        t.managed = true;
        t
    }

    fn wait_until(t: &mut TermSession, f: impl Fn(&mut TermSession) -> bool, ms: u64) -> bool {
        for _ in 0..(ms / 20) {
            if f(t) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        false
    }

    #[test]
    fn prompt_sent_when_ready_reaches_the_shell_and_completes() {
        let mut t = sh();
        // Espera o prompt do shell aparecer (CLI "pronta").
        assert!(
            wait_until(&mut t, |t| looks_idle(&t.screen_text()), 4_000),
            "o shell deveria mostrar um prompt; tela:\n{}",
            t.screen_text()
        );
        assert_eq!(t.send_prompt("echo marcador_unico"), PromptDelivery::Sent);
        // O texto foi escrito, mas só é SUBMETIDO quando aparece na tela.
        assert_eq!(t.state(), CliState::Delivering);

        // O tick confirma a entrega, submete, e depois detecta a conclusão.
        let done = {
            let mut tail = None;
            for _ in 0..300 {
                if let Some(t) = t.tick() {
                    tail = Some(t);
                    break;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            tail
        };
        let tail = done.expect("a conclusão deveria ser detectada");
        assert!(
            tail.contains("marcador_unico"),
            "a tela deveria conter o comando executado; veio:\n{tail}"
        );
        assert_eq!(t.state(), CliState::Idle);
        // Conclusão é reportada só uma vez.
        assert!(t.tick().is_none());
    }

    #[test]
    fn prompt_before_ready_is_deferred_and_delivered_later() {
        // `sleep` não desenha prompt nenhum: força o caminho de espera e o
        // teto de arranque não é atingido no tempo do teste.
        let mut t = TermSession::spawn_env(
            "dormindo".into(),
            "sh",
            &["-c".into(), "sleep 0.6; exec sh".into()],
            std::path::Path::new("/tmp"),
            24,
            80,
            &[],
        )
        .expect("spawn");
        t.managed = true;
        // Ainda subindo: o prompt é guardado, não escrito.
        assert_eq!(t.send_prompt("echo depois"), PromptDelivery::Deferred);
        assert!(t.pending_prompt.is_some());

        // Quando o shell aparece, o tick entrega sozinho.
        assert!(
            wait_until(
                &mut t,
                |t| {
                    t.tick();
                    t.pending_prompt.is_none()
                },
                5_000
            ),
            "o prompt guardado deveria ser entregue; tela:\n{}",
            t.screen_text()
        );
        assert!(wait_until(
            &mut t,
            |t| t.screen_text().contains("echo depois"),
            5_000
        ));
    }

    #[test]
    fn prompt_is_only_submitted_after_it_shows_up_on_screen() {
        // O envio às cegas perdia o texto quando a CLI ainda desenhava a
        // interface: o Enter saía antes do paste ser processado.
        let mut t = sh();
        assert!(wait_until(&mut t, |t| looks_idle(&t.screen_text()), 4_000));
        t.send_prompt("echo assinatura_unica_do_teste");
        assert_eq!(t.state(), CliState::Delivering);

        // Enquanto o texto não aparece, nada é submetido.
        let confirmou = wait_until(
            &mut t,
            |t| {
                t.tick();
                t.state() == CliState::Working
            },
            6_000,
        );
        assert!(confirmou, "tela:\n{}", t.screen_text());
        // E o que foi submetido é o que pedimos.
        assert!(wait_until(
            &mut t,
            |t| t.history(200).iter().any(|l| l.contains("assinatura_unica_do_teste")),
            6_000
        ));
    }

    #[test]
    fn delivery_is_confirmed_by_a_changed_screen_too() {
        // CLIs resumem paste grande ("[Pasted text #1 +40 lines]"), então
        // procurar o texto literal não basta: a tela ter mudado conta.
        let mut t = sh();
        assert!(wait_until(&mut t, |t| looks_idle(&t.screen_text()), 4_000));
        t.send_prompt("echo confirmado_por_mudanca");
        assert_eq!(t.state(), CliState::Delivering);
        assert!(
            wait_until(
                &mut t,
                |t| {
                    t.tick();
                    t.state() == CliState::Working
                },
                6_000
            ),
            "tela:\n{}",
            t.screen_text()
        );
    }

    #[test]
    fn giving_up_on_delivery_tells_the_orchestrator() {
        // `stty -echo` + sleep: o terminal não ecoa e a aplicação não
        // desenha nada, então a entrega nunca se confirma — é o caso real de
        // uma CLI presa (esperando confirmação, travada no arranque).
        let mut t = TermSession::spawn_env(
            "surda".into(),
            "sh",
            &["-c".into(), "stty -echo; sleep 30".into()],
            std::path::Path::new("/tmp"),
            24,
            80,
            &[],
        )
        .unwrap();
        t.managed = true;
        // Espera o `stty -echo` valer antes de escrever.
        std::thread::sleep(Duration::from_millis(700));
        t.state = CliState::Idle; // pula a espera de arranque
        t.send_prompt("nada vai ecoar isto");
        let mut aviso = None;
        for _ in 0..200 {
            if let Some(msg) = t.tick() {
                aviso = Some(msg);
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        let aviso = aviso.expect("deveria avisar que não conseguiu entregar");
        assert!(aviso.contains("não consegui entregar"), "{aviso}");
        assert!(aviso.contains("surda"));
    }

    #[test]
    fn delivery_signature_is_the_tail_because_the_input_box_scrolls() {
        // Prompt longo: o começo sai de vista na caixa; o fim fica no cursor.
        let longo = format!("{} FIM_DO_PROMPT", "palavra ".repeat(60));
        let sig = delivery_signature(&longo);
        assert_eq!(sig.chars().count(), SIGNATURE_CHARS);
        assert!(sig.ends_with("FIM_DO_PROMPT"), "assinatura: {sig:?}");
        // Curto: a assinatura é o texto inteiro normalizado.
        assert_eq!(delivery_signature("  oi   mundo\n"), "oi mundo");
        assert_eq!(normalize_screen("a\n\n b   c"), "a b c");
    }

    #[test]
    fn exit_during_work_reports_once() {
        let mut t = TermSession::spawn_env(
            "morre".into(),
            "sh",
            &["-c".into(), "exit 0".into()],
            std::path::Path::new("/tmp"),
            24,
            80,
            &[],
        )
        .expect("spawn");
        t.managed = true;
        t.state = CliState::Working;
        assert!(
            wait_until(&mut t, |t| t.is_exited(), 4_000),
            "o processo deveria terminar"
        );
        // Morreu no meio do turno: reporta uma vez para o orquestrador saber.
        assert!(t.tick().is_some());
        assert_eq!(t.state(), CliState::Exited);
        assert!(t.tick().is_none());
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn has_active_children_sees_a_background_process_in_the_session() {
        // O shell some sem prompt (`wait`) enquanto o filho roda em segundo
        // plano — o caso real de um shell/subagente trabalhando em silêncio
        // que o QUIET_HARD, sozinho, confundiria com "concluído".
        let mut t = TermSession::spawn_env(
            "filhos".into(),
            "sh",
            &["-c".into(), "sleep 2 & wait".into()],
            std::path::Path::new("/tmp"),
            24,
            80,
            &[],
        )
        .expect("spawn");
        t.managed = true;
        assert!(
            wait_until(&mut t, |t| t.has_active_children(), 1_000),
            "o sleep em segundo plano deveria aparecer como filho vivo da sessão"
        );
        // Termina o filho: some da sessão, e o processo principal sai também.
        assert!(wait_until(&mut t, |t| t.is_exited(), 4_000));
        assert!(!t.has_active_children());
    }

    #[test]
    fn screen_tail_limits_lines_and_drops_blanks() {
        let mut t = sh();
        assert!(wait_until(&mut t, |t| !t.screen_text().trim().is_empty(), 4_000));
        let tail = t.screen_tail(2);
        assert!(tail.lines().count() <= 2);
        assert!(tail.lines().all(|l| !l.trim().is_empty()));
    }

    #[test]
    fn unmanaged_terminal_never_notifies() {
        let mut t = sh();
        t.managed = false;
        t.state = CliState::Working;
        for _ in 0..5 {
            assert!(t.tick().is_none());
        }
    }

    /// Processos vivos num grupo, lendo `/proc` — por NÚMERO, nunca por nome.
    ///
    /// Varrer processos por padrão de nome já derrubou a TUI, a sessão que a
    /// hospedava e o aplicativo do usuário; aqui o grupo é o único critério.
    fn no_grupo(pgid: i32) -> Vec<i32> {
        let mut achados = Vec::new();
        let Ok(dir) = std::fs::read_dir("/proc") else {
            return achados;
        };
        for entrada in dir.flatten() {
            let nome = entrada.file_name();
            let Ok(pid) = nome.to_string_lossy().parse::<i32>() else {
                continue;
            };
            let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) else {
                continue;
            };
            // O nome do programa vem entre parênteses e pode conter espaço:
            // os campos confiáveis começam depois do ÚLTIMO ')'.
            let Some((_, depois)) = stat.rsplit_once(')') else {
                continue;
            };
            let mut campos = depois.split_whitespace();
            let estado = campos.next().unwrap_or("");
            let grupo: i32 = campos.nth(1).and_then(|v| v.parse().ok()).unwrap_or(0);
            // Zumbi já morreu, só não foi colhido.
            if grupo == pgid && estado != "Z" {
                achados.push(pid);
            }
        }
        achados
    }

    /// Espera a condição virar verdade, devolvendo o que ela viu por último.
    fn ate<T>(mut f: impl FnMut() -> T, pronto: impl Fn(&T) -> bool, ms: u64) -> T {
        let mut visto = f();
        let limite = Instant::now() + Duration::from_millis(ms);
        while !pronto(&visto) && Instant::now() < limite {
            std::thread::sleep(Duration::from_millis(50));
            visto = f();
        }
        visto
    }

    #[test]
    fn the_guard_never_lets_us_signal_our_own_group() {
        let meu = unsafe { libc::getpgrp() };
        // Na chamada do sistema, 0 significa "o MEU grupo" e -1 "todos os
        // processos": os dois números que fariam a TUI se matar junto — e
        // levar com ela a sessão que a hospeda.
        assert!(!grupo_sinalizavel(0, meu));
        assert!(!grupo_sinalizavel(-1, meu));
        assert!(!grupo_sinalizavel(meu, meu));
        // 1 é o init.
        assert!(!grupo_sinalizavel(1, meu));
        // Um grupo alheio qualquer, sim.
        assert!(grupo_sinalizavel(meu + 1, meu));
    }

    /// Processos VIVOS de uma sessão, lidos de `/proc` por número.
    fn vivos_na_sessao(sid: i32) -> Vec<i32> {
        let mut achados = Vec::new();
        let Ok(dir) = std::fs::read_dir("/proc") else {
            return achados;
        };
        for entrada in dir.flatten() {
            let Ok(pid) = entrada.file_name().to_string_lossy().parse::<i32>() else {
                continue;
            };
            let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) else {
                continue;
            };
            let Some((_, depois)) = stat.rsplit_once(')') else {
                continue;
            };
            let campos: Vec<&str> = depois.split_whitespace().collect();
            if campos.len() > 3 && campos[0] != "Z" && campos[3].parse() == Ok(sid) {
                achados.push(pid);
            }
        }
        achados
    }

    fn cli(nome: &str, script: &str) -> TermSession {
        TermSession::spawn_env(
            nome.into(),
            "sh",
            &["-c".into(), script.into()],
            std::path::Path::new("/tmp"),
            24,
            80,
            &[],
        )
        .expect("spawn")
    }

    #[test]
    fn closing_a_cli_takes_even_a_grandchild_that_ignores_sighup() {
        // O vazamento REAL. Um neto comum (`sleep &`) já morria antes desta
        // mudança: quando o líder da sessão morre, o kernel manda SIGHUP ao
        // grupo em primeiro plano — medido. Quem sobrevivia era o que ignora
        // SIGHUP, como um servidor subido com `nohup`: com o `kill` antigo
        // ele ficava vivo (medido: 1 sobrevivente).
        let mut t = cli("nohup", "nohup sleep 47 >/dev/null 2>&1 & sleep 47");
        let sid = t.pid.expect("pid") as i32;
        let vivos = ate(|| vivos_na_sessao(sid), |v| v.len() >= 2, 5_000);
        assert!(vivos.len() >= 2, "a CLI e o neto deviam estar vivos: {vivos:?}");

        t.kill();

        let restantes = ate(|| vivos_na_sessao(sid), Vec::is_empty, 3_000);
        assert!(restantes.is_empty(), "sobrou processo da CLI: {restantes:?}");
    }

    #[test]
    fn a_background_job_in_its_own_group_is_reached_too() {
        // Com controle de jobs (`set -m`, o normal num shell interativo) o
        // job em segundo plano ganha GRUPO PRÓPRIO: nem o grupo do filho nem
        // o de primeiro plano o alcançam — só a sessão, que ele não deixa.
        let mut t = cli("jobs", "set -m; nohup sleep 48 >/dev/null 2>&1 & sleep 48");
        let sid = t.pid.expect("pid") as i32;
        let vivos = ate(|| vivos_na_sessao(sid), |v| v.len() >= 2, 5_000);
        assert!(vivos.len() >= 2, "vivos: {vivos:?}");
        // Confirma que o cenário é o que diz ser: há membro fora do grupo
        // do filho (senão este teste não provaria nada).
        let fora_do_grupo = vivos.iter().any(|p| !no_grupo(sid).contains(p));
        assert!(fora_do_grupo, "o job devia ter grupo próprio; vivos={vivos:?}");

        t.kill();

        let restantes = ate(|| vivos_na_sessao(sid), Vec::is_empty, 3_000);
        assert!(restantes.is_empty(), "o job de grupo próprio sobreviveu: {restantes:?}");
    }

    #[test]
    fn a_cli_that_exits_is_seen_as_exited_even_if_a_grandchild_holds_the_terminal() {
        // O neto ignora SIGHUP e continua segurando o PTY: sem EOF no master,
        // a thread leitora nunca soube que a CLI saiu e o card seguia "vivo".
        let t = cli("sai", "trap '' HUP; sleep 49 & exit 0");
        let sid = t.pid.expect("pid") as i32;
        let saiu = ate(|| t.is_exited(), |e| *e, 3_000);
        drop(t); // o kill do Drop tem de levar o neto junto
        let restantes = ate(|| vivos_na_sessao(sid), Vec::is_empty, 3_000);
        assert!(saiu, "a CLI saiu mas o card não soube");
        assert!(restantes.is_empty(), "o neto sobreviveu ao fechamento: {restantes:?}");
    }

    #[test]
    fn a_grandchild_that_calls_setsid_escapes_this_and_it_is_known() {
        // Este teste FIXA UM LIMITE, não um desejo: `setsid` põe o neto numa
        // sessão nova, fora de tudo que o `kill` alcança sem varrer processos
        // por nome. Se algum dia isso for resolvido (cgroup próprio por CLI,
        // por exemplo), este teste falha — e a resposta certa é atualizá-lo.
        let dir = tempfile::tempdir().expect("tempdir");
        let marca = dir.path().join("pid");
        let mut t = cli(
            "fugitivo",
            &format!("setsid sh -c 'echo $$ > {}; exec sleep 47' & sleep 47", marca.display()),
        );
        let conteudo = ate(
            || std::fs::read_to_string(&marca).unwrap_or_default(),
            |s| s.trim().parse::<i32>().is_ok(),
            5_000,
        );
        let fugitivo: i32 = conteudo.trim().parse().expect("pid do neto");

        t.kill();

        // Virou líder da própria sessão: é por ela que se confere.
        let sobreviveu = !ate(|| vivos_na_sessao(fugitivo), Vec::is_empty, 1_500).is_empty();
        // Limpeza ANTES do assert (senão uma falha deixaria o sleep vivo), e
        // só depois de conferir que o número ainda é daquela sessão.
        if vivos_na_sessao(fugitivo).contains(&fugitivo) {
            unsafe { libc::kill(fugitivo, libc::SIGKILL) };
        }
        assert!(
            sobreviveu,
            "o limite documentado mudou: o neto com setsid morreu junto — \
             atualize o comentário do `kill` e este teste"
        );
    }
}

#[cfg(test)]
mod history_tests {
    use super::*;

    fn sh_running(cmd: &str) -> TermSession {
        let mut t = TermSession::spawn_env(
            "hist".into(),
            "sh",
            &["-c".into(), cmd.into()],
            std::path::Path::new("/tmp"),
            24,
            80,
            &[],
        )
        .unwrap();
        t.managed = true;
        t
    }

    fn wait_for(t: &TermSession, needle: &str, ms: u64) -> bool {
        for _ in 0..(ms / 20) {
            if t.history(2000).iter().any(|l| l.contains(needle)) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        false
    }

    #[test]
    fn history_keeps_lines_that_scrolled_off_the_screen() {
        // 60 linhas numa tela de 24: a primeira sai da vista.
        let t = sh_running("echo MARCA_INICIAL; for i in $(seq 1 60); do echo enchendo $i; done");
        assert!(wait_for(&t, "enchendo 60", 5_000), "saída não chegou");

        // Fora da tela…
        assert!(!t.screen_text().contains("MARCA_INICIAL"));
        // …mas no histórico sim.
        let h = t.history(2000);
        assert!(
            h.iter().any(|l| l.contains("MARCA_INICIAL")),
            "o histórico deveria guardar o que saiu da tela"
        );
        // E vem na ordem certa.
        let pos_marca = h.iter().position(|l| l.contains("MARCA_INICIAL")).unwrap();
        let pos_fim = h.iter().position(|l| l.contains("enchendo 60")).unwrap();
        assert!(pos_marca < pos_fim);
    }

    #[test]
    fn search_returns_only_the_matching_region() {
        let t = sh_running("echo antes; echo ERRO_AQUI; echo depois; for i in $(seq 1 40); do echo ruido $i; done");
        assert!(wait_for(&t, "ruido 40", 5_000));
        let hits = t.search("erro_aqui", 1, 10);
        assert!(hits.iter().any(|l| l.contains("ERRO_AQUI")));
        assert!(hits.iter().any(|l| l.contains("antes")), "contexto antes");
        assert!(hits.iter().any(|l| l.contains("depois")), "contexto depois");
        // O ponto: NÃO traz o resto do histórico.
        assert!(!hits.iter().any(|l| l.contains("ruido 20")));
        assert!(hits.len() <= 4, "trecho enxuto, veio: {hits:?}");
    }

    #[test]
    fn search_without_matches_is_empty() {
        let t = sh_running("echo so isso");
        assert!(wait_for(&t, "so isso", 5_000));
        assert!(t.search("nao_existe_mesmo", 2, 5).is_empty());
    }

    #[test]
    fn history_is_capped_and_keeps_the_newest() {
        let t = sh_running("for i in $(seq 1 2500); do echo item $i; done");
        assert!(wait_for(&t, "item 2500", 10_000));
        let h = t.history(HISTORY_MAX_LINES * 2);
        assert!(h.len() <= HISTORY_MAX_LINES, "sem teto: {} linhas", h.len());
        assert!(h.iter().any(|l| l.contains("item 2500")), "perdeu o fim");
    }

    #[test]
    fn strip_ansi_leaves_plain_text() {
        assert_eq!(strip_ansi("\u{1b}[31mvermelho\u{1b}[0m"), "vermelho");
        assert_eq!(strip_ansi("\u{1b}]0;titulo\u{7}texto"), "texto");
        assert_eq!(strip_ansi("sem escape"), "sem escape");
        // Sequência de posicionamento some, conteúdo fica.
        assert_eq!(strip_ansi("\u{1b}[2J\u{1b}[Hola"), "ola");
    }

    #[test]
    fn scrolling_past_the_screen_height_does_not_panic() {
        // `set_scrollback` além da altura estoura dentro do vt100 e derruba a
        // TUI — a roda do mouse chegava lá com poucos giros.
        let mut t = sh_running("for i in $(seq 1 200); do echo linha $i; done; sleep 5");
        assert!(wait_for(&t, "linha 200", 5_000));
        for _ in 0..50 {
            t.scroll_by(3);
            let _ = t.screen_text();
        }
        for _ in 0..80 {
            t.scroll_by(-3);
            let _ = t.screen_text();
        }
        assert_eq!(safe_scrollback(999, 24), 24);
        assert_eq!(safe_scrollback(5, 24), 5);
    }
}


// ---------------------------------------------------------------- menus

/// Uma opção de um menu interativo na tela da CLI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MenuOption {
    /// Linha da tela onde ela está (0 = topo).
    pub row: u16,
    /// Texto da opção, sem o marcador de seleção.
    pub text: String,
    /// É a opção sob o cursor agora?
    pub selected: bool,
    /// Aceita texto livre ("outra", "custom", "outro valor...").
    pub free_text: bool,
}

/// O que a CLI está pedindo na tela: um menu de escolha, com as opções e
/// qual delas está marcada.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Menu {
    /// Pergunta acima das opções, quando dá para identificar.
    pub question: String,
    pub options: Vec<MenuOption>,
}

impl Menu {
    /// Índice da opção selecionada agora.
    pub fn selected_index(&self) -> Option<usize> {
        self.options.iter().position(|o| o.selected)
    }

    /// Índice da opção cujo texto casa `alvo` (sem caixa/acentuação de
    /// marcador). Aceita o número da opção ("3") e casamento parcial.
    pub fn find(&self, alvo: &str) -> Option<usize> {
        let alvo = alvo.trim().to_lowercase();
        if alvo.is_empty() {
            return None;
        }
        // "3" escolhe a terceira opção (menus costumam numerar).
        if let Ok(n) = alvo.parse::<usize>() {
            if n >= 1 && n <= self.options.len() {
                return Some(n - 1);
            }
        }
        let norm = |s: &str| s.to_lowercase();
        self.options
            .iter()
            .position(|o| norm(&o.text) == alvo)
            .or_else(|| self.options.iter().position(|o| norm(&o.text).contains(&alvo)))
    }

    /// Passos de seta para sair da opção atual e chegar em `destino`:
    /// negativo sobe, positivo desce.
    pub fn steps_to(&self, destino: usize) -> i32 {
        let atual = self.selected_index().unwrap_or(0);
        destino as i32 - atual as i32
    }
}

/// Pausa entre as setas ao navegar um menu (a TUI redesenha a cada uma).
const MENU_KEY_DELAY: Duration = Duration::from_millis(120);
/// Pausa antes de escrever numa opção de texto livre recém-escolhida.
const FREE_TEXT_DELAY: Duration = Duration::from_millis(400);

/// Marcadores que as CLIs usam para indicar a opção sob o cursor.
const SELECTION_MARKERS: &[&str] = &["❯", "▸", "▶", "►", ">", "»", "→", "*", "●", "◉"];

/// Palavras que denunciam uma opção de resposta livre.
const FREE_TEXT_HINTS: &[&str] = &[
    "outra", "outro", "other", "custom", "personaliz", "digite", "escrever",
    "especific", "livre",
];

impl TermSession {
    /// Lê a tela como um MENU: quais opções existem e qual está marcada.
    ///
    /// O texto puro não basta — é o que o orquestrador enxergava antes, e por
    /// isso ele não conseguia responder um questionário: dá para ler as
    /// alternativas, mas não para saber onde o cursor está. Aqui usamos os
    /// ATRIBUTOS de cada célula do emulador (vídeo invertido, negrito, cor de
    /// fundo) além dos marcadores textuais, que é como o destaque realmente
    /// aparece num terminal.
    pub fn menu(&self) -> Option<Menu> {
        let guard = self.parser();
        let tela = guard.screen();
        let (linhas, colunas) = tela.size();

        let mut options: Vec<MenuOption> = Vec::new();
        let mut ultima_pergunta = String::new();
        let mut pergunta_do_menu = String::new();

        for row in 0..linhas {
            let mut texto = String::new();
            let mut destacada = false;
            for col in 0..colunas {
                if let Some(cell) = tela.cell(row, col) {
                    texto.push_str(&cell.contents());
                    // Uma célula com conteúdo e destaque marca a linha.
                    if !cell.contents().trim().is_empty()
                        && (cell.inverse() || cell.bgcolor() != vt100::Color::Default)
                    {
                        destacada = true;
                    }
                }
            }
            let bruto = texto.trim_end().to_string();
            let limpo = bruto.trim();
            if limpo.is_empty() {
                continue;
            }
            match strip_marker(limpo) {
                Some((marcada, corpo)) if !corpo.trim().is_empty() => {
                    if options.is_empty() {
                        pergunta_do_menu = ultima_pergunta.clone();
                    }
                    let corpo = corpo.trim().to_string();
                    let free_text = is_free_text(&corpo);
                    options.push(MenuOption {
                        row,
                        text: corpo,
                        selected: marcada || destacada,
                        free_text,
                    });
                }
                _ => {
                    // Linha comum: candidata a pergunta do próximo menu.
                    if options.is_empty() {
                        ultima_pergunta = limpo.to_string();
                    }
                }
            }
        }

        if options.len() < 2 {
            return None;
        }
        // Sem nenhuma marcada, o destaque não foi detectável: assume a 1ª,
        // e quem for escolher navega a partir dali.
        if !options.iter().any(|o| o.selected) {
            options[0].selected = true;
        }
        Some(Menu {
            question: pergunta_do_menu,
            options,
        })
    }

    /// Manda uma tecla de navegação para a CLI (setas, Enter, Esc, Tab...).
    pub fn send_key(&mut self, key: NavKey) {
        self.write_bytes(key.bytes());
        self.touch_activity();
    }

    /// Escolhe uma opção do menu que está na tela.
    ///
    /// Navega com as setas a partir de onde o cursor está até a opção pedida
    /// e confirma. É o que faltava para responder questionário: com
    /// `send_prompt` dava para digitar, mas não para ESCOLHER.
    ///
    /// `resposta` preenche uma opção de texto livre ("outra: ____"): a opção
    /// é selecionada, confirmada, e então o texto é digitado e submetido.
    pub fn choose(&mut self, alvo: &str, resposta: Option<&str>) -> Result<String> {
        let menu = self
            .menu()
            .context("não vejo um menu de escolha na tela desta CLI agora")?;
        let idx = menu.find(alvo).ok_or_else(|| {
            anyhow::anyhow!(
                "não achei a opção \"{alvo}\". Opções na tela: {}",
                menu.options
                    .iter()
                    .enumerate()
                    .map(|(i, o)| format!("{}) {}", i + 1, o.text))
                    .collect::<Vec<_>>()
                    .join(" · ")
            )
        })?;
        let passos = menu.steps_to(idx);
        let tecla = if passos < 0 { NavKey::Up } else { NavKey::Down };
        for _ in 0..passos.abs() {
            self.send_key(tecla);
            // As TUIs redesenham a cada movimento; um respiro evita perder
            // tecla em interface lenta.
            std::thread::sleep(MENU_KEY_DELAY);
        }
        let escolhida = menu.options[idx].text.clone();
        let livre = menu.options[idx].free_text;
        self.send_key(NavKey::Enter);
        self.state = CliState::Working;
        self.sent_at = Some(Instant::now());

        match resposta {
            Some(texto) if !texto.trim().is_empty() => {
                // Deixa a CLI desenhar o campo antes de escrever nele.
                std::thread::sleep(FREE_TEXT_DELAY);
                self.send_prompt(texto);
                Ok(format!(
                    "escolhi \"{escolhida}\" ({} passo(s)) e respondi \"{}\"",
                    passos.abs(),
                    texto.trim()
                ))
            }
            _ if livre => Ok(format!(
                "escolhi \"{escolhida}\", que pede uma resposta escrita — mande \
                 cli_choose de novo com o campo `resposta`, ou cli_send com o texto"
            )),
            _ => Ok(format!(
                "escolhi \"{escolhida}\" ({} passo(s) + Enter)",
                passos.abs()
            )),
        }
    }
}

/// Teclas que o orquestrador pode precisar para responder um menu.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NavKey {
    Up,
    Down,
    Left,
    Right,
    Enter,
    Esc,
    Tab,
    Space,
    Backspace,
}

impl NavKey {
    /// Bytes que um terminal envia para esta tecla.
    pub fn bytes(self) -> &'static [u8] {
        match self {
            NavKey::Up => b"\x1b[A",
            NavKey::Down => b"\x1b[B",
            NavKey::Right => b"\x1b[C",
            NavKey::Left => b"\x1b[D",
            NavKey::Enter => b"\r",
            NavKey::Esc => b"\x1b",
            NavKey::Tab => b"\t",
            NavKey::Space => b" ",
            NavKey::Backspace => &[0x7f],
        }
    }

    /// Interpreta o nome que o orquestrador manda na tool.
    pub fn parse(nome: &str) -> Option<Self> {
        match nome.trim().to_lowercase().as_str() {
            "up" | "cima" | "↑" => Some(NavKey::Up),
            "down" | "baixo" | "↓" => Some(NavKey::Down),
            "left" | "esquerda" | "←" => Some(NavKey::Left),
            "right" | "direita" | "→" => Some(NavKey::Right),
            "enter" | "return" | "ok" => Some(NavKey::Enter),
            "esc" | "escape" | "cancelar" => Some(NavKey::Esc),
            "tab" => Some(NavKey::Tab),
            "space" | "espaco" | "espaço" => Some(NavKey::Space),
            "backspace" | "apagar" => Some(NavKey::Backspace),
            _ => None,
        }
    }
}

/// Separa o marcador de seleção do corpo da opção.
///
/// Devolve `(está_marcada, texto)` quando a linha parece uma opção de menu:
/// começa com marcador (`❯ Sim`), com numeração (`3. Outra`), ou com caixa
/// de seleção (`[x] Ativar`, `(•) Não`).
pub fn strip_marker(linha: &str) -> Option<(bool, String)> {
    let t = linha.trim_start();
    // Marcador de cursor.
    for m in SELECTION_MARKERS {
        if let Some(resto) = t.strip_prefix(m) {
            if resto.starts_with(' ') || resto.is_empty() {
                let resto = resto.trim_start();
                // O marcador pode vir antes da numeração: "❯ 2. Não".
                let corpo = strip_numbering(resto);
                return Some((true, corpo));
            }
        }
    }
    // Caixa de seleção: [x]/[ ]/(•)/( ).
    if let Some(resto) = t.strip_prefix('[').or_else(|| t.strip_prefix('(')) {
        if resto.len() >= 2 {
            let marcada = !matches!(resto.chars().next(), Some(' '));
            if let Some(fim) = resto.find([']', ')']) {
                let corpo = resto[fim + 1..].trim().to_string();
                if !corpo.is_empty() {
                    return Some((marcada, strip_numbering(&corpo)));
                }
            }
        }
    }
    // Numeração pura: "1. Sim", "2) Não", "3 - Outra".
    let corpo = strip_numbering(t);
    if corpo != t && !corpo.is_empty() {
        return Some((false, corpo));
    }
    None
}

/// Remove numeração de opção do começo do texto ("3 - ", "2. ", "1) ").
fn strip_numbering(texto: &str) -> String {
    let t = texto.trim_start();
    let digitos: String = t.chars().take_while(char::is_ascii_digit).collect();
    if digitos.is_empty() || digitos.len() > 2 {
        return t.to_string();
    }
    let resto = t[digitos.len()..].trim_start();
    for sep in [".", ")", "-", ":"] {
        if let Some(r) = resto.strip_prefix(sep) {
            return r.trim_start().to_string();
        }
    }
    t.to_string()
}

/// A opção aceita uma resposta escrita pelo usuário?
pub fn is_free_text(texto: &str) -> bool {
    let t = texto.to_lowercase();
    FREE_TEXT_HINTS.iter().any(|h| t.contains(h))
}

/// Leitura de MENU: o orquestrador via o texto do terminal, mas não sabia
/// qual opção estava selecionada nem como escolher outra — era o que
/// impedia ele de responder um questionário na CLI.
#[cfg(test)]
mod menu_tests {
    use super::*;

    /// Monta uma sessão cuja tela é exatamente o texto dado.
    ///
    /// `stty -echo` é essencial: sem isso o terminal ecoa o que escrevemos E
    /// o `cat` imprime de volta, e a tela sai duplicada.
    fn tela(conteudo: &str) -> TermSession {
        let mut t = TermSession::spawn_env(
            "menu".into(),
            "sh",
            &["-c".into(), "stty -echo; cat".into()],
            std::path::Path::new("/tmp"),
            24,
            80,
            &[],
        )
        .unwrap();
        std::thread::sleep(Duration::from_millis(300));
        t.write_bytes(conteudo.as_bytes());
        for _ in 0..100 {
            if !t.screen_text().trim().is_empty() {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        std::thread::sleep(Duration::from_millis(120));
        t
    }

    #[test]
    fn reads_options_and_which_one_is_selected_by_marker() {
        let t = tela("Confia nos servidores MCP deste projeto?\r\n\
                      ❯ 1. Sim, usar e confiar nos futuros\r\n\
                        2. Sim, só nesta vez\r\n\
                        3. Não usar\r\n");
        let m = t.menu().expect("deveria ver um menu");
        assert_eq!(m.options.len(), 3);
        assert_eq!(m.options[0].text, "Sim, usar e confiar nos futuros");
        assert_eq!(m.selected_index(), Some(0));
        assert!(m.question.contains("Confia"), "pergunta: {:?}", m.question);
    }

    #[test]
    fn detects_selection_by_highlight_not_only_by_marker() {
        // Muitas TUIs destacam com vídeo invertido em vez de marcador —
        // sem ler atributo, o orquestrador não sabe onde está o cursor.
        let t = tela("Escolha:\r\n  1. Alfa\r\n\x1b[7m  2. Beta\x1b[0m\r\n  3. Gama\r\n");
        let m = t.menu().expect("menu");
        assert_eq!(m.selected_index(), Some(1), "opções: {:?}", m.options);
        assert_eq!(m.options[1].text, "Beta");
    }

    #[test]
    fn finds_option_by_text_number_or_partial_match() {
        let t = tela("  1. Sim\r\n  2. Não\r\n  3. Outra resposta\r\n");
        let m = t.menu().unwrap();
        assert_eq!(m.find("Não"), Some(1));
        assert_eq!(m.find("nao"), None, "sem acento não casa exato");
        assert_eq!(m.find("3"), Some(2), "número escolhe a opção");
        assert_eq!(m.find("outra"), Some(2), "casamento parcial");
        assert_eq!(m.find("inexistente"), None);
    }

    #[test]
    fn marks_free_text_options() {
        let t = tela("  1. Sim\r\n  2. Não\r\n  4 - Outra resposta\r\n");
        let m = t.menu().unwrap();
        assert!(!m.options[0].free_text);
        assert!(
            m.options[2].free_text,
            "‘outra resposta’ pede texto livre: {:?}",
            m.options[2]
        );
    }

    #[test]
    fn steps_to_walks_from_the_current_selection() {
        let t = tela("❯ 1. Um\r\n  2. Dois\r\n  3. Três\r\n");
        let m = t.menu().unwrap();
        assert_eq!(m.steps_to(2), 2, "desce duas");
        let t2 = tela("  1. Um\r\n  2. Dois\r\n❯ 3. Três\r\n");
        let m2 = t2.menu().unwrap();
        assert_eq!(m2.steps_to(0), -2, "sobe duas");
    }

    #[test]
    fn plain_text_is_not_a_menu() {
        let t = tela("Compilando o projeto\r\nerro: faltou ponto e vírgula\r\n");
        assert!(t.menu().is_none(), "saída comum não é menu");
    }

    #[test]
    fn strip_marker_handles_the_shapes_clis_use() {
        assert_eq!(strip_marker("❯ Sim"), Some((true, "Sim".into())));
        assert_eq!(strip_marker("  2. Não"), Some((false, "Não".into())));
        assert_eq!(strip_marker("3 - Outra"), Some((false, "Outra".into())));
        assert_eq!(strip_marker("[x] Ativar"), Some((true, "Ativar".into())));
        assert_eq!(strip_marker("[ ] Desativar"), Some((false, "Desativar".into())));
        assert_eq!(strip_marker("❯ 2) Talvez"), Some((true, "Talvez".into())));
        // Texto comum não vira opção.
        assert_eq!(strip_marker("compilando..."), None);
        assert_eq!(strip_marker("2026 foi um ano"), None);
    }

    #[test]
    fn nav_keys_map_to_terminal_bytes() {
        assert_eq!(NavKey::parse("baixo"), Some(NavKey::Down));
        assert_eq!(NavKey::parse("ENTER"), Some(NavKey::Enter));
        assert_eq!(NavKey::parse("↑"), Some(NavKey::Up));
        assert_eq!(NavKey::parse("inventada"), None);
        assert_eq!(NavKey::Down.bytes(), b"\x1b[B");
        assert_eq!(NavKey::Enter.bytes(), b"\r");
    }

    #[test]
    fn plain_input_gets_raw_text_not_paste_escapes() {
        // Campo simples (`read` do shell) não entende bracketed paste: os
        // escapes entrariam como texto na resposta.
        let mut t = TermSession::spawn_env(
            "campo".into(),
            "sh",
            &["-c".into(), "read -r x; echo \"LEU: $x\"; sleep 5".into()],
            std::path::Path::new("/tmp"),
            24,
            80,
            &[],
        )
        .unwrap();
        t.managed = true;
        std::thread::sleep(Duration::from_millis(500));
        t.state = CliState::Idle;
        t.send_prompt("resposta personalizada");
        for _ in 0..80 {
            t.tick();
            if t.screen_text().contains("LEU:") {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        let tela = t.screen_text();
        assert!(
            tela.contains("LEU: resposta personalizada"),
            "o campo deveria receber o texto limpo; tela:\n{tela}"
        );
        assert!(
            !tela.contains("200~"),
            "escapes de paste vazaram para o campo:\n{tela}"
        );
    }

    #[test]
    fn choosing_an_absent_option_lists_what_exists() {
        let mut t = tela("  1. Sim\r\n  2. Não\r\n");
        let err = t.choose("talvez", None).unwrap_err().to_string();
        assert!(err.contains("não achei"), "{err}");
        assert!(err.contains("Sim") && err.contains("Não"), "{err}");
    }

    #[test]
    fn choosing_without_a_menu_says_so() {
        let mut t = tela("apenas texto\r\n");
        let err = t.choose("qualquer", None).unwrap_err().to_string();
        assert!(err.contains("menu de escolha"), "{err}");
    }
}


#[cfg(test)]
mod tap_tests {
    use super::*;

    fn shell(dir: &Path) -> TermSession {
        TermSession::spawn_env("tap".into(), "sh", &[], dir, 24, 80, &[]).expect("sh no PTY")
    }

    fn receber_ate(rx: &Receiver<Vec<u8>>, alvo: &str, ms: u64) -> String {
        let fim = Instant::now() + Duration::from_millis(ms);
        let mut visto = Vec::new();
        while Instant::now() < fim {
            if let Ok(pedaco) = rx.recv_timeout(Duration::from_millis(50)) {
                visto.extend(pedaco);
                if String::from_utf8_lossy(&visto).contains(alvo) {
                    break;
                }
            }
        }
        String::from_utf8_lossy(&visto).into_owned()
    }

    fn esperar_tela(t: &TermSession, alvo: &str) -> bool {
        let fim = Instant::now() + Duration::from_secs(5);
        while Instant::now() < fim {
            if t.screen_text().lines().any(|l| l.trim() == alvo) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(30));
        }
        false
    }

    #[test]
    fn subscribers_get_the_current_screen_then_only_new_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let mut t = shell(dir.path());
        // `printf` com o texto partido: a linha digitada não contém a marca,
        // só a SAÍDA contém.
        t.write_bytes(b"printf 'marca-%s\\n' antes\r");
        assert!(esperar_tela(&t, "marca-antes"), "{}", t.screen_text());

        let (foto, rx) = t.subscribe_output();
        assert!(
            String::from_utf8_lossy(&foto).contains("marca-antes"),
            "a foto deveria trazer o que já estava na tela"
        );
        t.write_bytes(b"printf 'marca-%s\\n' depois\r");
        let depois = receber_ate(&rx, "marca-depois", 5000);
        assert!(depois.contains("marca-depois"), "{depois:?}");
        assert!(!depois.contains("marca-antes"), "o que veio na foto não se repete: {depois:?}");
        t.kill();
    }

    #[test]
    fn a_subscriber_that_goes_away_does_not_break_the_others() {
        let dir = tempfile::tempdir().unwrap();
        let mut t = shell(dir.path());
        let (_, rx_saiu) = t.subscribe_output();
        let (_, rx) = t.subscribe_output();
        drop(rx_saiu);
        t.write_bytes(b"printf 'viva-%s\\n' ainda\r");
        let visto = receber_ate(&rx, "viva-ainda", 5000);
        assert!(visto.contains("viva-ainda"), "{visto:?}");
        t.kill();
    }
}
