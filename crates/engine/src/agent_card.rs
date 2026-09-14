//! Card de agente controlado pelo ORQUESTRADOR.
//!
//! Diferente dos terminais PTY (uso manual do usuário), aqui o agente é
//! dirigido pelo orquestrador via subprocesso `claude -p --output-format
//! stream-json` — sem teclado, sem PTY. O card EXIBE o progresso e aceita
//! turnos de acompanhamento (`--resume`): o usuário digita um novo prompt
//! e o mesmo agente continua a sessão, iterando individualmente.
//!
//! **Modo autônomo** (`auto`): quando ligado, o App continua a sessão
//! sozinho rumo à meta do projeto, um passo por turno, até um teto de
//! iterações — pausando enquanto houver uma decisão crítica na fila
//! (hook `ask-regex`), retomando após o usuário aprovar em F2.

use std::path::PathBuf;
use std::sync::mpsc::Receiver;
use std::time::Instant;

use orchestrator_cli_adapter::claude_code::stream::{
    ApiStreamEvent, StreamEvent, ToolCallTracker,
};
use orchestrator_cli_adapter::claude_code::ClaudeCodeAgent;
use orchestrator_cli_adapter::AgentOptions;

use crate::chat::ChatEvent;

/// Marcador que o agente emite (1ª linha) quando considera a meta atingida.
pub const DONE_SENTINEL: &str = "ORCHESTRATOR_DONE";

/// Parâmetros para abrir um [`AgentCard`].
pub struct AgentSpec {
    pub name: String,
    pub task: String,
    /// Projeto ao qual o card pertence (para casar decisões pendentes).
    pub project: String,
    pub project_dir: PathBuf,
    pub options: AgentOptions,
    /// Banco de memória exportado ao subprocesso (`ORCHESTRATOR_DB`) para o
    /// hook/MCP acharem a fila de decisões certa.
    pub db_path: Option<PathBuf>,
    /// Postura autônoma: exporta `ORCHESTRATOR_AUTONOMOUS=1` (hook é a
    /// autoridade). As posturas que confirmam deixam `false`.
    pub autonomous: bool,
    /// Liga o modo autônomo desde o início.
    pub auto: bool,
    /// Teto de iterações autônomas (proteção de custo).
    pub max_iterations: usize,
    /// Meta que guia a continuação autônoma (vazia = usa a tarefa inicial).
    pub goal: String,
}

/// Um agente rodando sob controle do orquestrador, exibido como card.
pub struct AgentCard {
    /// Identificação exibida (ex.: "Agente #1").
    pub name: String,
    /// Última tarefa/prompt dado ao agente.
    pub task: String,
    /// Projeto do card (casa com decisões pendentes por projeto/sessão).
    pub project: String,
    /// Saída acumulada (texto + marcadores de ferramenta).
    pub output: String,
    /// Terminou o turno atual? (aceita follow-up quando `true`).
    pub done: bool,
    pub is_error: bool,
    /// Um turno está em andamento agora?
    pub busy: bool,
    pub session_id: Option<String>,
    /// Rótulo curto das opções aplicadas (modelo/effort/mode), p/ exibição.
    pub opts_label: String,
    /// Prompt de acompanhamento sendo digitado no card.
    pub input: String,
    /// Follow-up enfileirado durante um turno em andamento (Enter com o
    /// agente ocupado). Enviado automaticamente quando o turno terminar,
    /// preservando a sessão (`--resume`) e portanto o contexto.
    pub queued: Option<String>,
    /// Modo autônomo ligado?
    pub auto: bool,
    /// Iterações autônomas já feitas.
    pub iterations: usize,
    /// Teto de iterações autônomas.
    pub max_iterations: usize,
    /// A meta foi declarada concluída pelo agente (sentinela)?
    pub goal_done: bool,
    /// Pensamento parcial do turn em andamento (esmaecido no card).
    pub thinking: String,
    /// Tokens de entrada do último turn (cumulativos).
    pub tokens_in: u64,
    /// Tokens de saída do último turn (cumulativos).
    pub tokens_out: u64,
    /// Custo em USD do último turn, quando reportado.
    pub cost: Option<f64>,
    /// Deslocamento de scroll a partir do fim (0 = acompanhando a saída).
    pub scroll_from_end: u16,
    /// Início do turno em andamento (a UI mostra o tempo correndo).
    pub busy_since: Option<Instant>,
    /// Meta que guia a continuação autônoma.
    goal: String,
    /// Texto do último resultado (para detectar a sentinela de conclusão).
    last_result: String,
    /// Configuração do agente (modelo/effort/permission mode) — reusada
    /// nos turnos de acompanhamento.
    agent: ClaudeCodeAgent,
    project_dir: PathBuf,
    rx: Receiver<ChatEvent>,
}

impl AgentCard {
    /// Dispara a tarefa em background e devolve o card. `options` vira flags
    /// da CLI só quando a build alvo as suporta (checado em `run_turn`).
    pub fn spawn(spec: AgentSpec) -> Self {
        let opts_label = options_label(&spec.options);
        let goal = if spec.goal.trim().is_empty() {
            spec.task.clone()
        } else {
            spec.goal
        };
        let agent = ClaudeCodeAgent {
            options: spec.options,
            db_path: spec.db_path,
            project_name: Some(spec.project.clone()),
            autonomous: spec.autonomous,
            ..Default::default()
        };
        let rx = run_turn_thread(
            agent.clone(),
            spec.project_dir.clone(),
            spec.task.clone(),
            None,
        );
        Self {
            name: spec.name,
            task: spec.task,
            project: spec.project,
            output: String::new(),
            done: false,
            is_error: false,
            busy: true,
            session_id: None,
            opts_label,
            input: String::new(),
            queued: None,
            auto: spec.auto,
            iterations: 0,
            max_iterations: spec.max_iterations.max(1),
            goal_done: false,
            thinking: String::new(),
            tokens_in: 0,
            tokens_out: 0,
            cost: None,
            scroll_from_end: 0,
            busy_since: Some(Instant::now()),
            goal,
            last_result: String::new(),
            agent,
            project_dir: spec.project_dir,
            rx,
        }
    }

    /// Continua a sessão com um novo prompt (`--resume`). Só funciona quando
    /// não há turno em andamento e já existe `session_id`.
    pub fn send_followup(&mut self, prompt: String) -> bool {
        let prompt = prompt.trim().to_string();
        if prompt.is_empty() || self.busy {
            return false;
        }
        let Some(session) = self.session_id.clone() else {
            self.output
                .push_str("\n✖ sem session_id ainda — aguarde o primeiro turno concluir.");
            return false;
        };
        self.output.push_str(&format!("\n\n▸ você: {prompt}\n"));
        self.task = prompt.clone();
        self.done = false;
        self.is_error = false;
        self.busy = true;
        self.busy_since = Some(Instant::now());
        self.thinking.clear();
        self.tokens_in = 0;
        self.tokens_out = 0;
        self.cost = None;
        self.rx = run_turn_thread(
            self.agent.clone(),
            self.project_dir.clone(),
            prompt,
            Some(session),
        );
        true
    }

    /// Enfileira um follow-up digitado DURANTE um turno em andamento; será
    /// enviado por [`drain`](Self::drain) assim que o turno terminar. Um novo
    /// Enter enquanto ainda ocupado acrescenta à mesma mensagem (não perde).
    pub fn queue_followup(&mut self, prompt: String) {
        let prompt = prompt.trim().to_string();
        if prompt.is_empty() {
            return;
        }
        match &mut self.queued {
            Some(q) => {
                q.push('\n');
                q.push_str(&prompt);
            }
            None => self.queued = Some(prompt),
        }
    }

    /// O App deve continuar este card autonomamente agora?
    ///
    /// Verdadeiro quando: auto ligado, turno anterior concluído sem erro,
    /// meta ainda não declarada concluída, sessão já conhecida e teto de
    /// iterações não atingido. A checagem de decisão pendente (pausa) é
    /// feita pelo App, que tem acesso ao store.
    pub fn wants_autocontinue(&self) -> bool {
        self.auto
            && self.done
            && !self.busy
            && !self.is_error
            && !self.goal_done
            && self.session_id.is_some()
            && self.iterations < self.max_iterations
    }

    /// Prompt de continuação autônoma: um passo concreto rumo à meta, ou a
    /// sentinela de conclusão. Decisão de política DO ORQUESTRADOR.
    pub fn continuation_prompt(&self) -> String {
        format!(
            "Objetivo: {goal}\n\n\
             Continue autonomamente rumo a esse objetivo. Faça o PRÓXIMO passo \
             concreto (apenas um passo por turno) e verifique o resultado. Se o \
             objetivo já estiver plenamente atingido e verificado, responda com a \
             PRIMEIRA LINHA sendo exatamente `{DONE_SENTINEL}` e nada mais depois. \
             Se surgir uma decisão que só o usuário pode tomar, explique em uma \
             frase e pare.",
            goal = self.goal
        )
    }

    /// Drena eventos da thread do agente. Chame a cada tick da UI.
    pub fn drain(&mut self) {
        while let Ok(event) = self.rx.try_recv() {
            match event {
                ChatEvent::Delta(d) => self.output.push_str(&d),
                ChatEvent::Thinking(d) => self.thinking.push_str(&d),
                ChatEvent::Tokens {
                    input,
                    output,
                    cost,
                } => {
                    self.tokens_in = input;
                    self.tokens_out = output;
                    if cost.is_some() {
                        self.cost = cost;
                    }
                }
                ChatEvent::Done {
                    session_id,
                    text,
                    is_error,
                } => {
                    if session_id.is_some() {
                        self.session_id = session_id;
                    }
                    self.last_result = text.clone();
                    self.output.push_str(&text);
                    self.done = true;
                    self.busy = false;
                    self.busy_since = None;
                    self.is_error = is_error;
                    self.thinking.clear();
                    // Agente sinalizou conclusão da meta: desliga o autopilot.
                    if self.last_result.contains(DONE_SENTINEL) {
                        self.goal_done = true;
                        self.auto = false;
                    }
                }
                ChatEvent::Failed(msg) => {
                    self.output.push_str(&format!("\n✖ {msg}"));
                    self.last_result = msg;
                    self.done = true;
                    self.busy = false;
                    self.busy_since = None;
                    self.is_error = true;
                }
            }
        }
        // Mensagem enfileirada durante o turno: envia agora que ele acabou.
        // Tem prioridade sobre a continuação autônoma (é a voz do usuário).
        if self.done && !self.busy && self.session_id.is_some() {
            if let Some(prompt) = self.queued.take() {
                self.send_followup(prompt);
            }
        }
    }
}

/// Rótulo curto das opções aplicadas, p/ o título do card (vazio se nada).
fn options_label(o: &AgentOptions) -> String {
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
    if o.permission_prompt_tool.is_some() {
        parts.push("ask-tool".into());
    }
    parts.join(" ")
}

/// Roda um turno do agente numa thread própria (runtime tokio dedicado),
/// reencaminhando os eventos como [`ChatEvent`] pelo canal devolvido.
fn run_turn_thread(
    agent: ClaudeCodeAgent,
    project_dir: PathBuf,
    prompt: String,
    resume: Option<String>,
) -> Receiver<ChatEvent> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let rt = match tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        {
            Ok(rt) => rt,
            Err(e) => {
                let _ = tx.send(ChatEvent::Failed(format!("runtime tokio: {e}")));
                return;
            }
        };
        rt.block_on(async move {
            let mut rx_events = match agent
                .run_turn(&project_dir, &prompt, resume.as_deref())
                .await
            {
                Ok(rx) => rx,
                Err(e) => {
                    let _ = tx.send(ChatEvent::Failed(format!(
                        "não consegui iniciar `claude`: {e}"
                    )));
                    return;
                }
            };
            let mut text = String::new();
            // Detalhe das tool calls (comando/arquivo/diff): o JSON de
            // entrada chega fatiado e fecha no `content_block_stop`.
            let mut tools = ToolCallTracker::new();
            let mut session: Option<String> = None;
            let mut got_result = false;
            let (mut ti, mut to) = (0u64, 0u64);
            while let Some(event) = rx_events.recv().await {
                match event {
                    StreamEvent::SystemInit { session_id, .. } => {
                        session = Some(session_id);
                    }
                    StreamEvent::Stream(ApiStreamEvent::TextDelta { text: delta }) => {
                        text.push_str(&delta);
                        let _ = tx.send(ChatEvent::Delta(delta));
                    }
                    StreamEvent::Stream(ApiStreamEvent::ThinkingDelta { text: delta }) => {
                        let _ = tx.send(ChatEvent::Thinking(delta));
                    }
                    StreamEvent::Stream(ApiStreamEvent::Usage {
                        input_tokens,
                        output_tokens,
                    }) => {
                        if let Some(v) = input_tokens {
                            ti = v;
                        }
                        if let Some(v) = output_tokens {
                            to = v;
                        }
                        let _ = tx.send(ChatEvent::Tokens {
                            input: ti,
                            output: to,
                            cost: None,
                        });
                    }
                    StreamEvent::Stream(ApiStreamEvent::ContentBlockStart {
                        index,
                        tool_use_name: Some(tool),
                        ..
                    }) => tools.start(index, &tool),
                    StreamEvent::Stream(ApiStreamEvent::ToolInputDelta {
                        index,
                        partial_json,
                    }) => tools.push(index, &partial_json),
                    StreamEvent::Stream(ApiStreamEvent::ContentBlockStop { index }) => {
                        if let Some(line) = tools.finish(index) {
                            let _ = tx.send(ChatEvent::Delta(format!("\n{line}\n")));
                        }
                    }
                    StreamEvent::Result {
                        session_id,
                        result,
                        is_error,
                        input_tokens,
                        output_tokens,
                        cost_usd,
                        ..
                    } => {
                        got_result = true;
                        let _ = tx.send(ChatEvent::Tokens {
                            input: input_tokens.unwrap_or(ti),
                            output: output_tokens.unwrap_or(to),
                            cost: cost_usd,
                        });
                        let final_text = result.unwrap_or_else(|| text.clone());
                        let _ = tx.send(ChatEvent::Done {
                            session_id: session_id.or(session.clone()),
                            text: format!("\n✔ concluído: {final_text}"),
                            is_error,
                        });
                    }
                    _ => {}
                }
            }
            if !got_result {
                let _ = tx.send(ChatEvent::Done {
                    session_id: session,
                    text: "\n✖ o processo terminou sem resultado".into(),
                    is_error: true,
                });
            }
        });
    });
    rx
}
