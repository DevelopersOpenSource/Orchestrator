//! Card de chat com o orquestrador (lado esquerdo do workbench).
//!
//! O provedor de LLM é escolhível (Ctrl+p): Claude Code CLI (subprocesso
//! `claude -p` + stream-json) ou qualquer endpoint compatível com OpenAI
//! (Ollama, LM Studio, Groq, OpenRouter, Perplexity, NVIDIA NIM, ...). No
//! modo CLI a sessão persiste com `--resume` e é restaurada entre execuções
//! da TUI (`ui_state`); no modo HTTP o histórico completo vai a cada chamada.
//!
//! O orquestrador NÃO escreve código: ele comanda CLIs reais pelas tools MCP
//! `cli_start`/`cli_send`/`cli_status`/`cli_stop` (ver [`SYSTEM`]). Mensagens
//! digitadas durante um turno em andamento entram numa FILA e são enviadas
//! sozinhas ao fim do turno — inclusive as notificações automáticas de
//! "a CLI X concluiu a tarefa".
//!
//! A memória do projeto NÃO é montada aqui: o índice, as regras fixas do
//! dono e as obrigações chegam a cada prompt pelo hook `UserPromptSubmit`,
//! que consulta o `orchestrator-memoryd` (ChromaDB + reranker) — o mesmo
//! texto que as CLIs recebem.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use orchestrator_cli_adapter::claude_code::stream::{
    ApiStreamEvent, StreamEvent, ToolCallTracker,
};
use orchestrator_cli_adapter::claude_code::ClaudeCodeAgent;
use orchestrator_cli_adapter::harness::{self, HarnessEvent, HarnessMessage, McpLaunch, TurnRequest};
use orchestrator_cli_adapter::{antigravity, codex, kimi, opencode};
use orchestrator_core::{LlmProvider, ProviderKind};
use orchestrator_llm::{chat_completion, ChatMessage};

/// Instruções fixas do orquestrador.
///
/// Define o papel: MAESTRO, não executor. O usuário foi explícito — o
/// orquestrador não deve escrever código na mão nem abrir subagentes; ele
/// abre CLIs reais nomeadas, manda tarefas e itera a cada conclusão.
pub const SYSTEM: &str = "Você é o Orchestrator: o maestro deste projeto. \
Responda em português, curto e direto.\n\n\
SEU PAPEL: você NÃO escreve código, NÃO edita arquivos e NÃO abre \
subagentes. Você comanda CLIs de agente REAIS, que aparecem como cards na \
tela do usuário. Ler o repositório para entender o estado é permitido e \
desejável; produzir a mudança é trabalho das CLIs.\n\n\
COMO TRABALHAR:\n\
1. Delegue abrindo uma CLI com `cli_start`, com nome que diga o papel dela \
(ex.: \"frontend\", \"backend\", \"testes\"). Reaproveite uma CLI já aberta \
quando o assunto for o mesmo — `cli_status` sem argumento lista as abertas.\n\
2. Mande a tarefa com `cli_send`: uma tarefa concreta por envio, com \
contexto suficiente para a CLI trabalhar sozinha.\n\
3. NÃO fique esperando nem chame `cli_status` em loop: quando a CLI \
terminar, você recebe automaticamente uma mensagem de sistema dizendo que \
ela concluiu. Só então olhe o resultado com `cli_status`.\n\
3b. QUANDO A CLI PERGUNTAR ALGO com alternativas (confirmação, questionário, \
escolha de arquivo), `cli_status` mostra o texto mas NÃO diz qual opção está \
sob o cursor: use `cli_menu` para ver as opções e a selecionada, e \
`cli_choose` para escolher pelo texto ou pelo número. Se a opção pedir \
resposta escrita (\"outra\"), mande `cli_choose` com o campo `resposta`. \
`cli_key` manda tecla crua (esc, tab, space) quando nada disso couber.\n\
4. Avalie e decida o próximo passo: mandar a continuação para a mesma CLI, \
abrir outra CLI, ou PARAR e perguntar ao usuário quando a decisão for dele \
(escopo, arquitetura, algo irreversível). Diga sempre o que você concluiu e \
o que vai fazer em seguida.\n\
5. TESTE SEMPRE antes de dizer que terminou — isto NÃO é opcional. Uma CLI \
dizer \"pronto\" não é prova de nada: rode você mesmo. Assim que houver algo \
para ver ou rodar (página, servidor, script, binário, AppImage), chame \
`ui_open` (ou `ui_exec`, que sobe a sandbox sozinho) — é um container \
isolado, não toca na máquina nem na tela do usuário, e o dono pode \
acompanhar ao vivo na aba \"Tela virtual\" do app enquanto você testa. A \
página volta como lista de elementos com referência: `ui_click e3`, `ui_type \
e5 \"texto\"`, `ui_snapshot` para reler. Se o comportamento não bater com o \
que os elementos mostram, ou algo parecer quebrado (layout, cor, um erro só \
visual), peça `ui_screenshot` e OLHE a imagem com a tool Read antes de \
concluir — não adivinhe. Achou problema? Mande a CLI corrigir e teste nesta \
MESMA sandbox de novo antes de dar por encerrado.\n\
6. Encerre com `cli_stop` a CLI cujo trabalho acabou, e `ui_stop` a sandbox \
(ela some sozinha da \"Tela virtual\").\n\n\
ECONOMIA DE CONTEXTO: `cli_status` devolve um resumo curto de propósito. \
Quando precisar de mais, use `cli_read` com `search` (ex.: \"error\", \
\"FAILED\", o nome do arquivo) em vez de pedir a tela inteira. E NUNCA \
repita a saída do terminal na sua resposta: diga em uma frase o que \
aconteceu e o que você vai fazer.\n\n\
DECISÕES NO MODO AUTÔNOMO: quando uma CLI pedir confirmação por uma regra \
de segurança, o pedido chega a você como mensagem de sistema. Decida você \
mesmo com `decision_resolve`: aprove o que bate com o que o dono pediu e é \
seguro; negue com o motivo o que não bate; depois avise a CLI com `cli_send`. \
Só passe ao dono com `decision_escalate` o que for MUITO crítico \
(irreversível sem autorização clara), fugir do pedido, ou quando você não \
souber decidir. Quando a escolha for do dono (escopo, arquitetura, \
preferência), pergunte com `ask_owner`, com alternativas curtas; não pare \
esperando a resposta: siga outra frente.\n\n\
CONTEXTO E SEGURANÇA: você conhece as memórias do projeto (segurança, \
arquitetura, sintaxe, decisões) que chegam no bloco de contexto. Regras de \
segurança são IMPOSITIVAS: nunca proponha nada que as viole, nem instrua \
uma CLI a violá-las. Quando o usuário pedir para lembrar algo, sugira o \
comando `orchestrator memory add`.";

/// Prefixo das linhas de notificação automática do sistema no transcript.
pub const SYSTEM_WHO: &str = "sistema";

/// Eventos emitidos pelas threads de chat/agente para a UI.
pub enum ChatEvent {
    /// Texto incremental do assistente.
    Delta(String),
    /// Pensamento incremental (`thinking_delta`) — exibido esmaecido.
    Thinking(String),
    /// Contagem de tokens ao vivo (cumulativa) + custo quando conhecido.
    Tokens {
        input: u64,
        output: u64,
        cost: Option<f64>,
    },
    /// Turn concluído.
    Done {
        session_id: Option<String>,
        text: String,
        is_error: bool,
    },
    /// Falha ao rodar o subprocesso/chamada HTTP.
    Failed(String),
}

/// Uma linha do transcript.
pub struct ChatLine {
    /// "você", "orchestrator", "sistema", "decisão" ou "erro".
    pub who: String,
    pub text: String,
}

impl ChatLine {
    pub fn new(who: impl Into<String>, text: impl Into<String>) -> Self {
        Self {
            who: who.into(),
            text: text.into(),
        }
    }

    /// Mensagem comprida o bastante para valer esconder até o usuário pedir.
    pub fn is_long(&self) -> bool {
        self.text.lines().count() > COLLAPSE_AFTER_LINES
    }

    /// O texto a exibir quando o transcript está compacto: as primeiras
    /// linhas e um aviso do que ficou escondido.
    pub fn preview(&self) -> String {
        let total = self.text.lines().count();
        let head: Vec<&str> = self.text.lines().take(COLLAPSE_AFTER_LINES).collect();
        format!(
            "{}\n… +{} linhas (Ctrl+e mostra tudo)",
            head.join("\n"),
            total - head.len()
        )
    }
}

/// A partir de quantas linhas uma mensagem é colapsada no transcript.
pub const COLLAPSE_AFTER_LINES: usize = 4;

/// Estado do card de chat.
pub struct ChatState {
    pub transcript: Vec<ChatLine>,
    pub input: String,
    pub busy: bool,
    /// Sessão do Claude CLI (`--resume`).
    pub session_id: Option<String>,
    /// Provedor ativo.
    pub provider: LlmProvider,
    /// Histórico para provedores HTTP (papéis user/assistant).
    history: Vec<ChatMessage>,
    /// Deslocamento de scroll a partir do fim (0 = acompanhando).
    pub scroll_from_end: u16,
    tx: Sender<ChatEvent>,
    rx: Receiver<ChatEvent>,
    /// Texto parcial do turn em andamento (deltas acumulados).
    partial: String,
    /// Pensamento parcial do turn em andamento (thinking_delta acumulado).
    thinking: String,
    /// Tokens de entrada do último turn (cumulativos).
    pub tokens_in: u64,
    /// Tokens de saída do último turn (cumulativos).
    pub tokens_out: u64,
    /// Custo em USD do último turn, quando reportado.
    pub cost: Option<f64>,
    /// Início do turno em andamento — a UI mostra o tempo correndo para o
    /// usuário saber que está vivo (e não travado).
    pub busy_since: Option<Instant>,
    /// Mensagem enfileirada durante um turno em andamento (Enter com o chat
    /// ocupado, ou notificação automática de CLI concluída). Enviada sozinha
    /// quando o turno terminar, preservando a sessão (`--resume`).
    pub queued: Option<String>,
    /// Argumentos extras do subprocesso `claude` (ex.: `--disallowed-tools`),
    /// montados pelo App conforme as capacidades da build.
    pub extra_args: Vec<String>,
    /// Variáveis da ferramenta, já resolvidas pelo Engine (ex.: endpoint e
    /// chave da Z.ai para o GLM pelo Claude Code).
    pub env: Vec<(String, String)>,
    /// Modelo escolhido pelo usuário (`/modelo`); vazio = o do provedor.
    pub model_override: String,
    /// Sessão da trava no servidor MCP: a consulta à memória vale para esta
    /// conversa inteira, turno após turno.
    gate_session: String,
    /// Mensagens já enviadas pelo usuário, para recuperar com ↑/↓.
    pub input_history: Vec<String>,
    /// Posição atual na navegação do histórico (None = digitando).
    hist_idx: Option<usize>,
    /// Rascunho guardado ao começar a navegar o histórico.
    hist_draft: String,
    /// Linhas novas do transcript ainda não persistidas (o App drena e grava
    /// em `chat_transcript`, para restaurar a conversa na próxima execução).
    to_persist: Vec<(String, String)>,
}

impl ChatState {
    pub fn new(provider: LlmProvider) -> Self {
        let (tx, rx) = std::sync::mpsc::channel();
        Self {
            transcript: Vec::new(),
            input: String::new(),
            busy: false,
            session_id: None,
            provider,
            history: Vec::new(),
            scroll_from_end: 0,
            tx,
            rx,
            partial: String::new(),
            thinking: String::new(),
            tokens_in: 0,
            tokens_out: 0,
            cost: None,
            busy_since: None,
            queued: None,
            extra_args: Vec::new(),
            env: Vec::new(),
            model_override: String::new(),
            gate_session: new_gate_session(),
            input_history: Vec::new(),
            hist_idx: None,
            hist_draft: String::new(),
            to_persist: Vec::new(),
        }
    }

    /// Texto parcial do assistente (turn em andamento), para renderização.
    pub fn partial(&self) -> &str {
        &self.partial
    }

    /// Pensamento parcial do turn em andamento, para renderização esmaecida.
    pub fn thinking(&self) -> &str {
        &self.thinking
    }

    /// Envia a mensagem digitada. `context` é o preâmbulo montado pelo App
    /// (workspace + índice de memórias + o que registrar) — ele depende da
    /// própria mensagem, por isso vem pronto de fora. `db_path` é exportado
    /// ao subprocesso `claude` (`ORCHESTRATOR_DB`) para o hook de segurança
    /// achar a fila de decisões certa.
    pub fn send(
        &mut self,
        context: String,
        project: &str,
        project_dir: PathBuf,
        db_path: Option<PathBuf>,
    ) {
        let message = self.input.trim().to_string();
        if message.is_empty() {
            return;
        }
        self.input.clear();
        self.reset_history_nav();
        // Ocupado: a mensagem NÃO se perde nem aborta o turno — entra na fila
        // e sai sozinha quando o turno atual acabar (ver `drain`).
        if self.busy {
            self.remember_input(&message);
            self.enqueue(&message);
            return;
        }
        self.remember_input(&message);
        self.push_line("você", message.clone());
        self.send_text(context, project, project_dir, db_path, message);
    }

    /// Envia um texto já pronto (sem passar pelo input). Usado pela fila e
    /// pelas notificações automáticas do sistema.
    pub fn send_text(
        &mut self,
        context: String,
        project: &str,
        project_dir: PathBuf,
        db_path: Option<PathBuf>,
        message: String,
    ) {
        if message.trim().is_empty() || self.busy {
            return;
        }
        self.busy = true;
        self.busy_since = Some(Instant::now());
        self.partial.clear();
        self.thinking.clear();
        self.tokens_in = 0;
        self.tokens_out = 0;
        self.cost = None;
        self.scroll_from_end = 0;

        match self.provider.kind {
            ProviderKind::ClaudeCli => {
                self.send_claude_cli(context, message, project.to_string(), project_dir, db_path);
            }
            ProviderKind::OpenAiCompat => {
                self.history.push(ChatMessage::user(message.clone()));
                self.send_openai_compat(context, message, project.to_string(), project_dir, db_path);
            }
            _ => self.send_harness(context, message, project.to_string(), project_dir, db_path),
        }
    }

    /// Troca quem responde. A conversa na tela continua; a sessão passa a ser
    /// a que este provedor já tinha (cada ferramenta guarda a sua) e o
    /// histórico HTTP recomeça.
    pub fn switch_provider(&mut self, provider: LlmProvider, session_id: Option<String>) {
        self.provider = provider;
        self.session_id = session_id;
        self.history.clear();
        self.gate_session = new_gate_session();
        self.push_line(SYSTEM_WHO, format!("agora com {}", self.provider.name));
    }

    /// Enfileira uma mensagem para o fim do turno atual. Enters seguidos
    /// acumulam na mesma mensagem (nada se perde).
    pub fn enqueue(&mut self, message: &str) {
        let message = message.trim();
        if message.is_empty() {
            return;
        }
        match &mut self.queued {
            Some(q) => {
                q.push('\n');
                q.push_str(message);
            }
            None => self.queued = Some(message.to_string()),
        }
    }

    /// Notificação automática do sistema (ex.: "a CLI X concluiu a tarefa").
    /// Aparece no transcript e vira o próximo prompt do orquestrador.
    pub fn notify_system(&mut self, text: String) {
        self.push_line(SYSTEM_WHO, text.clone());
        self.enqueue(&text);
    }

    /// Tira a mensagem da fila quando o chat está livre para enviá-la.
    pub fn take_queued(&mut self) -> Option<String> {
        if self.busy {
            return None;
        }
        self.queued.take()
    }

    /// Acrescenta uma linha ao transcript e a marca para persistência.
    pub fn push_line(&mut self, who: impl Into<String>, text: impl Into<String>) {
        let line = ChatLine::new(who, text);
        self.to_persist.push((line.who.clone(), line.text.clone()));
        self.transcript.push(line);
        self.scroll_from_end = 0;
    }

    /// Linhas ainda não gravadas em `chat_transcript` (o App drena e persiste).
    pub fn take_persist(&mut self) -> Vec<(String, String)> {
        std::mem::take(&mut self.to_persist)
    }

    /// Restaura transcript e histórico de input de uma execução anterior.
    /// As linhas restauradas NÃO voltam para a fila de persistência.
    pub fn restore(&mut self, lines: Vec<(String, String)>, session_id: Option<String>) {
        self.transcript = lines
            .into_iter()
            .map(|(who, text)| ChatLine::new(who, text))
            .collect();
        self.input_history = self
            .transcript
            .iter()
            .filter(|l| l.who == "você")
            .map(|l| l.text.clone())
            .collect();
        self.session_id = session_id;
        self.scroll_from_end = 0;
    }

    /// Limpa a conversa (comando `/nova`): novo contexto, nova sessão.
    pub fn reset(&mut self) {
        self.transcript.clear();
        self.history.clear();
        self.session_id = None;
        self.gate_session = new_gate_session();
        self.queued = None;
        self.partial.clear();
        self.thinking.clear();
        self.scroll_from_end = 0;
        self.reset_history_nav();
    }

    fn remember_input(&mut self, message: &str) {
        if self.input_history.last().map(String::as_str) != Some(message) {
            self.input_history.push(message.to_string());
        }
    }

    fn reset_history_nav(&mut self) {
        self.hist_idx = None;
        self.hist_draft.clear();
    }

    /// ↑ no chat: recupera a mensagem anterior (mantém o rascunho atual).
    pub fn history_prev(&mut self) {
        if self.input_history.is_empty() {
            return;
        }
        let next = match self.hist_idx {
            None => {
                self.hist_draft = self.input.clone();
                self.input_history.len() - 1
            }
            Some(0) => 0,
            Some(i) => i - 1,
        };
        self.hist_idx = Some(next);
        self.input = self.input_history[next].clone();
    }

    /// ↓ no chat: volta para mensagens mais recentes e, no fim, ao rascunho.
    pub fn history_next(&mut self) {
        let Some(i) = self.hist_idx else { return };
        if i + 1 >= self.input_history.len() {
            self.input = std::mem::take(&mut self.hist_draft);
            self.hist_idx = None;
        } else {
            self.hist_idx = Some(i + 1);
            self.input = self.input_history[i + 1].clone();
        }
    }

    fn send_claude_cli(
        &mut self,
        context: String,
        message: String,
        project: String,
        project_dir: PathBuf,
        db_path: Option<PathBuf>,
    ) {
        let prompt = if context.is_empty() {
            message
        } else {
            format!("{context}\n\n{message}")
        };
        let tx = self.tx.clone();
        let session_id = self.session_id.clone();
        // `--append-system-prompt` + o que o App montou (ex.: proibir as
        // tools de escrita, para o orquestrador delegar em vez de digitar).
        let mut extra_args = vec!["--append-system-prompt".to_string(), SYSTEM.to_string()];
        extra_args.extend(self.extra_args.iter().cloned());
        let env = self.env.clone();
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
                // O chat do orquestrador é SEMPRE gated pelo hook autoritativo
                // (`ORCHESTRATOR_AUTONOMOUS=1`): headless não tem canal para o
                // prompt nativo do `claude`, então sem isso qualquer tool que
                // precise de aprovação TRAVA o turn. Com o hook, tools limpas
                // são liberadas explicitamente e as que casam `ask-regex`/
                // `deny-regex` viram decisão pendente visível no chat/F2.
                let agent = ClaudeCodeAgent {
                    extra_args,
                    db_path,
                    project_name: Some(project),
                    autonomous: true,
                    env,
                    ..Default::default()
                };
                let mut rx_events = match agent
                    .run_turn(&project_dir, &prompt, session_id.as_deref())
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
                // Detalhe das tool calls: o JSON de entrada chega fatiado e
                // só fica completo no `content_block_stop`.
                let mut tools = ToolCallTracker::new();
                let mut got_result = false;
                // Tokens cumulativos do turn: message_start traz o input,
                // message_delta traz o output; guardamos o último conhecido.
                let (mut ti, mut to) = (0u64, 0u64);
                while let Some(event) = rx_events.recv().await {
                    match event {
                        StreamEvent::Stream(ApiStreamEvent::TextDelta { text: delta }) => {
                            text.push_str(&delta);
                            let _ = tx.send(ChatEvent::Delta(delta));
                        }
                        StreamEvent::Stream(ApiStreamEvent::ThinkingDelta { text: delta }) => {
                            let _ = tx.send(ChatEvent::Thinking(delta));
                        }
                        StreamEvent::Stream(ApiStreamEvent::ContentBlockStart {
                            index,
                            tool_use_name: Some(tool),
                            tool_use_id,
                            ..
                        }) => {
                            tools.start(index, &tool, tool_use_id.as_deref());
                        }
                        StreamEvent::Stream(ApiStreamEvent::ToolInputDelta {
                            index,
                            partial_json,
                        }) => tools.push(index, &partial_json),
                        StreamEvent::Stream(ApiStreamEvent::ContentBlockStop { index }) => {
                            // Tool call completa: a tarefa (Bash exige
                            // `description`), o arquivo escrito, o diff —
                            // o que o usuário pediu para conseguir ver.
                            if let Some(line) = tools.finish(index) {
                                let _ = tx.send(ChatEvent::Delta(format!("\n{line}\n")));
                            }
                        }
                        StreamEvent::ToolResults(results) => {
                            // Só o que falhou vira linha (sucesso não precisa
                            // de ruído extra): é o "identificar shells que
                            // falharam" — com a saída de verdade, não só o ✖.
                            for r in &results {
                                if let Some(line) = tools.describe_result(r) {
                                    let _ = tx.send(ChatEvent::Delta(format!("\n{line}\n")));
                                }
                            }
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
                            // Totais autoritativos do resultado (quando vierem).
                            let _ = tx.send(ChatEvent::Tokens {
                                input: input_tokens.unwrap_or(ti),
                                output: output_tokens.unwrap_or(to),
                                cost: cost_usd,
                            });
                            let final_text = result.unwrap_or_else(|| text.clone());
                            let _ = tx.send(ChatEvent::Done {
                                session_id,
                                text: final_text,
                                is_error,
                            });
                        }
                        _ => {}
                    }
                }
                if !got_result {
                    let _ = tx.send(ChatEvent::Done {
                        session_id: None,
                        text: if text.is_empty() {
                            "(sem resposta — o processo `claude` terminou sem resultado)".into()
                        } else {
                            text
                        },
                        is_error: true,
                    });
                }
            });
        });
    }

    /// Codex, Kimi Code, Antigravity e OpenCode: a ferramenta oficial de cada
    /// um, na conta do usuário, pelo mesmo laço.
    ///
    /// Sem hook de prompt nessas ferramentas, o contrato da memória vai no
    /// próprio prompt; as tools do Orchestrator vêm do servidor MCP em modo
    /// trava; e cada uma roda com o papel de maestro restrito do jeito dela
    /// (ver os módulos em `orchestrator_cli_adapter`).
    fn send_harness(
        &mut self,
        context: String,
        message: String,
        project: String,
        project_dir: PathBuf,
        db_path: Option<PathBuf>,
    ) {
        let kind = self.provider.kind;
        let tx = self.tx.clone();
        let binary = kind
            .binary()
            .map(|b| {
                crate::providers::Probe::current()
                    .find_binary(b)
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|| b.to_string())
            })
            .unwrap_or_default();
        let model = [&self.model_override, &self.provider.model]
            .into_iter()
            .find(|m| !m.trim().is_empty())
            .cloned();
        let session = self.session_id.clone();
        let gate_session = self.gate_session.clone();
        let extra_env = self.env.clone();
        let tool = kind.tool_label();
        std::thread::spawn(move || {
            let memoria = memory_contract(&project, &message, db_path.as_deref());
            let orq = orchestrator_env(&project, &project_dir, db_path.as_deref(), &gate_session, kind);
            let mcp = harness::sibling_binary("orchestrator-mcp").map(|command| McpLaunch {
                command,
                env: orq.clone(),
            });
            // O agy não tem onde receber instruções de sistema: vão no prompt.
            let mut partes: Vec<&str> = Vec::new();
            if kind == ProviderKind::AntigravityCli {
                partes.push(SYSTEM);
            }
            for p in [memoria.as_str(), context.as_str(), message.as_str()] {
                if !p.trim().is_empty() {
                    partes.push(p);
                }
            }
            let state_dir = orchestrator_core::config::project_dirs()
                .map(|d| d.cache_dir().join("ferramentas"))
                .unwrap_or_else(|_| std::env::temp_dir().join("orchestrator-ferramentas"));
            let mut env = extra_env;
            env.extend(orq);
            let req = TurnRequest {
                binary,
                dir: project_dir,
                prompt: partes.join("\n\n"),
                system: SYSTEM.to_string(),
                session,
                model,
                env,
                mcp: mcp.clone(),
                state_dir,
            };
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
                let iniciado = match kind {
                    ProviderKind::CodexCli => harness::run(codex::command(&req), codex::Parser).await,
                    ProviderKind::KimiCli => match kimi::command(&req) {
                        Ok(spec) => harness::run(spec, kimi::Parser::new(&req.dir)).await,
                        Err(e) => {
                            let _ = tx.send(ChatEvent::Failed(format!(
                                "não consegui preparar o agente do Kimi: {e}"
                            )));
                            return;
                        }
                    },
                    ProviderKind::OpencodeCli => {
                        harness::run(opencode::command(&req), opencode::Parser::default()).await
                    }
                    _ => harness::run(antigravity::command(&req), antigravity::Parser::default()).await,
                };
                let mut rx = match iniciado {
                    Ok(rx) => rx,
                    Err(e) => {
                        let _ = tx.send(ChatEvent::Failed(format!("não consegui iniciar o {tool}: {e}")));
                        return;
                    }
                };
                if mcp.is_none() {
                    let _ = tx.send(ChatEvent::Delta(
                        "(orchestrator-mcp não está ao lado do executável: sem as tools cli_*)\n".into(),
                    ));
                }
                let mut text = String::new();
                let mut errors: Vec<String> = Vec::new();
                let mut session_id: Option<String> = None;
                while let Some(msg) = rx.recv().await {
                    match msg {
                        HarnessMessage::Event(HarnessEvent::TextDelta(d)) => {
                            text.push_str(&d);
                            let _ = tx.send(ChatEvent::Delta(d));
                        }
                        HarnessMessage::Event(HarnessEvent::Text(t)) => {
                            let bloco = if text.is_empty() { t } else { format!("\n\n{t}") };
                            text.push_str(&bloco);
                            let _ = tx.send(ChatEvent::Delta(bloco));
                        }
                        HarnessMessage::Event(HarnessEvent::Thinking(t)) => {
                            let _ = tx.send(ChatEvent::Thinking(t));
                        }
                        HarnessMessage::Event(HarnessEvent::Tool(linha)) => {
                            let _ = tx.send(ChatEvent::Delta(format!("\n{linha}\n")));
                        }
                        HarnessMessage::Event(HarnessEvent::Tokens { input, output, cost }) => {
                            let _ = tx.send(ChatEvent::Tokens { input, output, cost });
                        }
                        HarnessMessage::Event(HarnessEvent::Session(id)) => session_id = Some(id),
                        HarnessMessage::Event(HarnessEvent::Error(e)) => errors.push(e),
                        HarnessMessage::End(fim) => {
                            let falhou = !fim.success || (text.trim().is_empty() && !errors.is_empty());
                            let final_text = if !text.trim().is_empty() {
                                text.clone()
                            } else if !errors.is_empty() {
                                errors.join("\n")
                            } else if !fim.stderr_tail.trim().is_empty() {
                                format!("o {tool} terminou sem resposta:\n{}", fim.stderr_tail)
                            } else {
                                format!("(sem resposta — o {tool} terminou sem dizer nada)")
                            };
                            let _ = tx.send(ChatEvent::Done {
                                session_id: session_id.clone(),
                                text: final_text,
                                is_error: falhou,
                            });
                        }
                    }
                }
            });
        });
    }

    /// Chat HTTP. Com ferramentas ligadas no provedor (e o `orchestrator-mcp`
    /// ao lado do executável), o modelo comanda CLIs como os outros; senão,
    /// só conversa.
    fn send_openai_compat(
        &mut self,
        context: String,
        message: String,
        project: String,
        project_dir: PathBuf,
        db_path: Option<PathBuf>,
    ) {
        let mcp_bin = if self.provider.tools_enabled() {
            harness::sibling_binary("orchestrator-mcp")
        } else {
            None
        };
        let Some(mcp_bin) = mcp_bin else {
            return self.send_plain_http(context);
        };
        let provider = self.provider.clone();
        let start = self.history.len().saturating_sub(20);
        let history: Vec<serde_json::Value> = self.history[start..]
            .iter()
            .map(|m| serde_json::json!({ "role": m.role, "content": m.content }))
            .collect();
        let tx = self.tx.clone();
        let gate_session = self.gate_session.clone();
        std::thread::spawn(move || {
            let api_key = match api_key_of(&provider) {
                Ok(k) => k,
                Err(msg) => {
                    let _ = tx.send(ChatEvent::Failed(msg));
                    return;
                }
            };
            let memoria = memory_contract(&project, &message, db_path.as_deref());
            let system = [SYSTEM, memoria.as_str(), context.as_str()]
                .into_iter()
                .filter(|p| !p.trim().is_empty())
                .collect::<Vec<_>>()
                .join("\n\n");
            let mut messages = vec![serde_json::json!({ "role": "system", "content": system })];
            messages.extend(history);
            let env = orchestrator_env(
                &project,
                &project_dir,
                db_path.as_deref(),
                &gate_session,
                ProviderKind::OpenAiCompat,
            );
            let mut mcp = match crate::mcp_client::SpawnedMcp::spawn(&mcp_bin, &env, &project_dir) {
                Ok(c) => c,
                Err(e) => {
                    let _ = tx.send(ChatEvent::Failed(format!("não consegui subir o servidor MCP: {e:#}")));
                    return;
                }
            };
            let tools = match mcp.list_tools() {
                Ok(t) => crate::mcp_client::openai_functions(&t),
                Err(e) => {
                    let _ = tx.send(ChatEvent::Failed(format!("o servidor MCP não listou as tools: {e:#}")));
                    return;
                }
            };
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
            let resultado = tool_loop(
                &mut messages,
                MAX_TOOL_ROUNDS,
                |msgs| {
                    rt.block_on(orchestrator_llm::chat_with_tools(
                        &provider.base_url,
                        api_key.as_deref(),
                        &provider.model,
                        msgs,
                        &tools,
                    ))
                },
                |nome, args| {
                    mcp.call_tool(nome, args)
                        .unwrap_or_else(|e| format!("ERRO ao chamar {nome}: {e:#}"))
                },
                |evento| {
                    let _ = tx.send(evento);
                },
            );
            let _ = tx.send(match resultado {
                Ok(text) => ChatEvent::Done {
                    session_id: None,
                    text,
                    is_error: false,
                },
                Err(e) => ChatEvent::Failed(format!("{}: {e}", provider.name)),
            });
        });
    }

    fn send_plain_http(&mut self, context: String) {
        let provider = self.provider.clone();
        // Sistema + contexto + últimas 20 mensagens do histórico.
        let mut messages = vec![ChatMessage::system(if context.is_empty() {
            SYSTEM.to_string()
        } else {
            format!("{SYSTEM}\n\n{context}")
        })];
        let start = self.history.len().saturating_sub(20);
        messages.extend_from_slice(&self.history[start..]);

        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let api_key = if provider.api_key_env.is_empty() {
                None
            } else {
                match std::env::var(&provider.api_key_env) {
                    Ok(k) if !k.trim().is_empty() => Some(k),
                    _ => {
                        let _ = tx.send(ChatEvent::Failed(format!(
                            "defina a variável de ambiente {} com a API key de {} \
                             (e reabra a TUI)",
                            provider.api_key_env, provider.name
                        )));
                        return;
                    }
                }
            };
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
                match chat_completion(
                    &provider.base_url,
                    api_key.as_deref(),
                    &provider.model,
                    &messages,
                )
                .await
                {
                    Ok(text) => {
                        let _ = tx.send(ChatEvent::Done {
                            session_id: None,
                            text,
                            is_error: false,
                        });
                    }
                    Err(e) => {
                        let _ = tx.send(ChatEvent::Failed(format!("{}: {e:#}", provider.name)));
                    }
                }
            });
        });
    }

    /// Drena eventos pendentes da thread de chat. Chame a cada tick da UI.
    pub fn drain(&mut self) {
        while let Ok(event) = self.rx.try_recv() {
            match event {
                ChatEvent::Delta(d) => self.partial.push_str(&d),
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
                    if let Some(id) = session_id {
                        self.session_id = Some(id);
                    }
                    if !is_error {
                        self.history.push(ChatMessage::assistant(text.clone()));
                    }
                    self.push_line(if is_error { "erro" } else { "orchestrator" }, text);
                    self.partial.clear();
                    self.thinking.clear();
                    self.busy = false;
                    self.busy_since = None;
                }
                ChatEvent::Failed(msg) => {
                    self.push_line("erro", msg);
                    self.partial.clear();
                    self.thinking.clear();
                    self.busy = false;
                    self.busy_since = None;
                }
            }
        }
    }
}

/// Teto de rodadas de ferramentas por turno no chat HTTP (proteção de custo:
/// cada rodada é uma chamada paga, e modelo pequeno pode entrar em laço).
pub const MAX_TOOL_ROUNDS: usize = 12;

/// O laço de ferramentas do chat HTTP, sem rede nem processo: `llm` é o
/// modelo, `exec` executa uma tool e `on_event` recebe o que mostrar.
/// Devolve o texto final, ou o erro.
pub(crate) fn tool_loop(
    messages: &mut Vec<serde_json::Value>,
    max_rounds: usize,
    mut llm: impl FnMut(&[serde_json::Value]) -> anyhow::Result<orchestrator_llm::AssistantTurn>,
    mut exec: impl FnMut(&str, &serde_json::Value) -> String,
    mut on_event: impl FnMut(ChatEvent),
) -> Result<String, String> {
    let (mut entrada, mut saida) = (0u64, 0u64);
    for _ in 0..max_rounds {
        let turno = llm(messages).map_err(|e| format!("{e:#}"))?;
        entrada += turno.input_tokens;
        saida += turno.output_tokens;
        on_event(ChatEvent::Tokens {
            input: entrada,
            output: saida,
            cost: None,
        });
        if turno.tool_calls.is_empty() {
            return Ok(turno.content);
        }
        if !turno.content.trim().is_empty() {
            on_event(ChatEvent::Delta(format!("{}\n", turno.content)));
        }
        messages.push(turno.raw_message.clone());
        for call in &turno.tool_calls {
            on_event(ChatEvent::Delta(format!(
                "\n{}\n",
                harness::tool_line(&call.name, &call.arguments)
            )));
            let args: serde_json::Value =
                serde_json::from_str(&call.arguments).unwrap_or_else(|_| serde_json::json!({}));
            let resultado = exec(&call.name, &args);
            messages.push(serde_json::json!({
                "role": "tool",
                "tool_call_id": call.id,
                "content": resultado,
            }));
        }
    }
    Err(format!(
        "parei depois de {max_rounds} rodadas de ferramentas sem resposta final \
         (proteção de custo) — mande a mensagem de novo para ele continuar"
    ))
}

/// A chave do provedor HTTP, da variável de ambiente dele.
fn api_key_of(provider: &LlmProvider) -> Result<Option<String>, String> {
    if provider.api_key_env.is_empty() {
        return Ok(None);
    }
    match std::env::var(&provider.api_key_env) {
        Ok(k) if !k.trim().is_empty() => Ok(Some(k)),
        _ => Err(format!(
            "defina a variável de ambiente {} com a chave de {} (e reabra o Orchestrator)",
            provider.api_key_env, provider.name
        )),
    }
}

/// O ambiente que o hook e o servidor MCP usam para saber banco, projeto,
/// trava e quem está chamando.
fn orchestrator_env(
    project: &str,
    dir: &Path,
    db: Option<&Path>,
    gate_session: &str,
    kind: ProviderKind,
) -> Vec<(String, String)> {
    let mut env: Vec<(String, String)> = vec![
        ("ORCHESTRATOR_PROJECT".into(), project.to_string()),
        ("ORCHESTRATOR_AGENT".into(), "orquestrador".into()),
        ("ORCHESTRATOR_AUTONOMOUS".into(), "1".into()),
        ("ORCHESTRATOR_GATE_IN_MCP".into(), "1".into()),
        ("ORCHESTRATOR_SESSION".into(), gate_session.to_string()),
        ("ORCHESTRATOR_HARNESS".into(), harness_name(kind).into()),
        ("ORCHESTRATOR_WORKDIR".into(), dir.display().to_string()),
    ];
    if let Some(db) = db {
        env.push(("ORCHESTRATOR_DB".into(), db.display().to_string()));
    }
    env
}

/// Id novo para a sessão da trava no MCP.
fn new_gate_session() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("chat-{}-{nanos:x}", std::process::id())
}

/// Como o hook e o servidor reconhecem cada ferramenta.
fn harness_name(kind: ProviderKind) -> &'static str {
    match kind {
        ProviderKind::ClaudeCli => "claude",
        ProviderKind::CodexCli => "codex",
        ProviderKind::KimiCli => "kimi",
        ProviderKind::AntigravityCli => "antigravity",
        ProviderKind::OpencodeCli => "opencode",
        ProviderKind::OpenAiCompat => "http",
    }
}

/// O texto invisível que o hook de prompt injeta no Claude Code, para as
/// ferramentas que não têm esse hook: contrato, regras do dono e o índice
/// escolhido para a mensagem. Pede ao memoryd; se ele não responde, dispara
/// o memoryd para os próximos turnos e monta pelo caminho léxico.
fn memory_contract(project: &str, prompt: &str, db: Option<&Path>) -> String {
    use orchestrator_memory::daemon::{self, Request};
    let pedido = Request::Context {
        project: project.to_string(),
        prompt: prompt.to_string(),
        author: "orquestrador".to_string(),
    };
    match daemon::call(&pedido, daemon::REQUEST_TIMEOUT) {
        Ok(resposta) => resposta.text,
        Err(_) => {
            let _ = daemon::spawn_detached();
            db.and_then(|d| orchestrator_memory::store::MemoryStore::open(d).ok())
                .and_then(|s| {
                    orchestrator_memory::contract::lexical_context(&s, project, prompt, "orquestrador").ok()
                })
                .unwrap_or_default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use orchestrator_core::LlmProvider;

    fn chat() -> ChatState {
        ChatState::new(LlmProvider {
            name: "teste".into(),
            kind: ProviderKind::ClaudeCli,
            base_url: String::new(),
            model: String::new(),
            api_key_env: String::new(),
            env: Default::default(),
            tools: None,
        })
    }

    fn pedido(id: &str, nome: &str, args: &str) -> orchestrator_llm::AssistantTurn {
        orchestrator_llm::AssistantTurn {
            tool_calls: vec![orchestrator_llm::ToolCall {
                id: id.into(),
                name: nome.into(),
                arguments: args.into(),
            }],
            input_tokens: 100,
            output_tokens: 5,
            raw_message: serde_json::json!({
                "role": "assistant", "content": null,
                "tool_calls": [{"id": id, "type": "function", "function": {"name": nome, "arguments": args}}]
            }),
            ..Default::default()
        }
    }

    #[test]
    fn tool_loop_runs_the_tools_and_returns_the_final_answer() {
        let mut messages = vec![
            serde_json::json!({"role": "system", "content": "maestro"}),
            serde_json::json!({"role": "user", "content": "abra uma CLI api"}),
        ];
        let mut rodada = 0;
        let mut chamadas: Vec<(String, serde_json::Value)> = Vec::new();
        let mut mostrado: Vec<String> = Vec::new();
        let r = tool_loop(
            &mut messages,
            MAX_TOOL_ROUNDS,
            |msgs| {
                rodada += 1;
                if rodada == 1 {
                    Ok(pedido("c1", "cli_start", r#"{"name":"api"}"#))
                } else {
                    // O resultado da tool voltou ao modelo, na ordem que a API exige.
                    let n = msgs.len();
                    assert_eq!(msgs[n - 2]["tool_calls"][0]["id"], "c1");
                    assert_eq!(msgs[n - 1]["role"], "tool");
                    assert_eq!(msgs[n - 1]["content"], "CLI api aberta");
                    Ok(orchestrator_llm::AssistantTurn {
                        content: "Abri a CLI api.".into(),
                        input_tokens: 130,
                        output_tokens: 7,
                        ..Default::default()
                    })
                }
            },
            |nome, args| {
                chamadas.push((nome.to_string(), args.clone()));
                "CLI api aberta".to_string()
            },
            |e| match e {
                ChatEvent::Delta(d) => mostrado.push(d),
                ChatEvent::Tokens { input, output, .. } => mostrado.push(format!("tokens {input}/{output}")),
                _ => {}
            },
        );
        assert_eq!(r.unwrap(), "Abri a CLI api.");
        assert_eq!(chamadas, vec![("cli_start".to_string(), serde_json::json!({"name": "api"}))]);
        assert!(mostrado.iter().any(|m| m.contains("⚙ cli_start")), "{mostrado:?}");
        assert!(mostrado.contains(&"tokens 230/12".to_string()), "{mostrado:?}");
    }

    #[test]
    fn tool_loop_stops_at_the_round_limit() {
        let mut messages = vec![serde_json::json!({"role": "user", "content": "x"})];
        let mut execucoes = 0;
        let r = tool_loop(
            &mut messages,
            3,
            |_| Ok(pedido("c", "cli_status", "{}")),
            |_, _| {
                execucoes += 1;
                "ok".into()
            },
            |_| {},
        );
        assert!(r.unwrap_err().contains("3 rodadas"));
        assert_eq!(execucoes, 3);
    }

    #[test]
    fn tool_loop_reports_model_errors() {
        let mut messages = Vec::new();
        let r = tool_loop(&mut messages, 5, |_| Err(anyhow::anyhow!("HTTP 429")), |_, _| String::new(), |_| {});
        assert_eq!(r.unwrap_err(), "HTTP 429");
    }

    #[test]
    fn switching_provider_keeps_the_screen_and_takes_its_own_session() {
        let mut c = chat();
        c.push_line("você", "oi");
        c.session_id = Some("sessao-claude".into());
        let outro = LlmProvider {
            name: "ChatGPT (Codex)".into(),
            kind: ProviderKind::CodexCli,
            ..c.provider.clone()
        };
        c.switch_provider(outro, None);
        assert_eq!(c.provider.name, "ChatGPT (Codex)");
        assert!(c.session_id.is_none(), "a sessão do Claude não serve ao Codex");
        assert_eq!(c.transcript.len(), 2);
        assert_eq!(c.transcript[1].who, SYSTEM_WHO);
        assert!(c.transcript[1].text.contains("ChatGPT"));
        // Aviso de troca não vira prompt.
        assert!(c.queued.is_none());
    }

    #[test]
    fn enqueue_accumulates_and_take_respects_busy() {
        let mut c = chat();
        c.busy = true;
        c.enqueue("primeira");
        c.enqueue("segunda");
        // Ocupado: a fila não é liberada.
        assert!(c.take_queued().is_none());
        c.busy = false;
        assert_eq!(c.take_queued().as_deref(), Some("primeira\nsegunda"));
        assert!(c.take_queued().is_none());
    }

    #[test]
    fn enqueue_ignores_blank() {
        let mut c = chat();
        c.enqueue("   \n  ");
        assert!(c.queued.is_none());
    }

    #[test]
    fn notify_system_shows_in_transcript_and_queues() {
        let mut c = chat();
        c.busy = true;
        c.notify_system("a CLI 'frontend' concluiu a tarefa".into());
        assert_eq!(c.transcript.len(), 1);
        assert_eq!(c.transcript[0].who, SYSTEM_WHO);
        c.busy = false;
        assert!(c.take_queued().unwrap().contains("frontend"));
    }

    #[test]
    fn history_navigates_and_keeps_draft() {
        let mut c = chat();
        c.input_history = vec!["um".into(), "dois".into()];
        c.input = "rascunho".into();
        c.history_prev();
        assert_eq!(c.input, "dois");
        c.history_prev();
        assert_eq!(c.input, "um");
        // Não passa do começo.
        c.history_prev();
        assert_eq!(c.input, "um");
        c.history_next();
        assert_eq!(c.input, "dois");
        // Voltando do fim, o rascunho reaparece.
        c.history_next();
        assert_eq!(c.input, "rascunho");
        c.history_next();
        assert_eq!(c.input, "rascunho");
    }

    #[test]
    fn history_without_entries_does_nothing() {
        let mut c = chat();
        c.input = "abc".into();
        c.history_prev();
        assert_eq!(c.input, "abc");
        c.history_next();
        assert_eq!(c.input, "abc");
    }

    #[test]
    fn restore_rebuilds_transcript_history_and_session() {
        let mut c = chat();
        c.restore(
            vec![
                ("você".into(), "oi".into()),
                ("orchestrator".into(), "olá".into()),
                ("você".into(), "abra uma cli".into()),
            ],
            Some("sess-1".into()),
        );
        assert_eq!(c.transcript.len(), 3);
        assert_eq!(c.input_history, vec!["oi", "abra uma cli"]);
        assert_eq!(c.session_id.as_deref(), Some("sess-1"));
        // Restauração não re-persiste o que já estava no banco.
        assert!(c.take_persist().is_empty());
    }

    #[test]
    fn push_line_marks_for_persistence() {
        let mut c = chat();
        c.push_line("você", "teste");
        let pend = c.take_persist();
        assert_eq!(pend, vec![("você".to_string(), "teste".to_string())]);
        assert!(c.take_persist().is_empty());
    }

    #[test]
    fn reset_clears_session_and_queue() {
        let mut c = chat();
        c.push_line("você", "oi");
        c.session_id = Some("s".into());
        c.enqueue("pendente");
        c.reset();
        assert!(c.transcript.is_empty());
        assert!(c.session_id.is_none());
        assert!(c.queued.is_none());
    }

    #[test]
    fn system_prompt_forbids_writing_and_teaches_the_cli_flow() {
        // Guard-rail do papel: se alguém afrouxar o prompt, o teste cai.
        assert!(SYSTEM.contains("NÃO escreve código"));
        assert!(SYSTEM.contains("cli_start"));
        assert!(SYSTEM.contains("cli_send"));
        assert!(SYSTEM.contains("cli_status"));
        // Autonomia: decide os pedidos das CLIs e só escala o crítico.
        assert!(SYSTEM.contains("decision_resolve"));
        assert!(SYSTEM.contains("decision_escalate"));
        assert!(SYSTEM.contains("ask_owner"));
    }
}
