import { useEffect, useRef, useState } from "react";
import { nucleo, type Card, type Foto, type Linha } from "./nucleo";
import { Terminal } from "./componentes/Terminal";
import { Paleta, type AcaoJanela } from "./componentes/Paleta";
import { Provedores } from "./componentes/Provedores";
import { Modelos } from "./componentes/Modelos";
import { Decisoes } from "./componentes/Decisoes";
import { Manual } from "./componentes/Manual";
import { Memoria } from "./componentes/Memoria";
import { Sandbox } from "./componentes/Sandbox";

type Vista = "workbench" | "decisoes" | "sandbox";

const ESTADO: Record<Card["estado"], [string, string]> = {
  iniciando: ["○", "iniciando"],
  trabalhando: ["●", "trabalhando"],
  ociosa: ["●", "pronta"],
  encerrada: ["✖", "encerrada"],
};
const TIPO: Record<Card["tipo"], string> = { cli: "CLI", terminal: "terminal", agente: "agente" };
const QUEM: Record<string, string> = { você: "você", orchestrator: "orquestrador", decisão: "decisão", sistema: "sistema", erro: "erro" };
const LINHAS_RESUMO = 12;

function Marca() {
  return (
    <svg className="marca-svg" viewBox="0 0 20 20" aria-hidden="true">
      <rect x="1" y="1" width="18" height="18" rx="5" className="m-fundo" />
      <circle cx="10" cy="10" r="4.2" className="m-anel" />
      <circle cx="15.2" cy="4.8" r="1.8" className="m-ponto" />
    </svg>
  );
}

function Mensagem({ linha }: { linha: Linha }) {
  const [inteira, setInteira] = useState(false);
  const linhas = linha.texto.split("\n");
  const longa = linhas.length > LINHAS_RESUMO;
  const texto = longa && !inteira ? linhas.slice(0, LINHAS_RESUMO).join("\n") : linha.texto;
  const papel = QUEM[linha.quem] ?? linha.quem;
  return (
    <div className={`msg msg-${linha.quem === "orchestrator" ? "orquestrador" : linha.quem === "você" ? "voce" : linha.quem}`}>
      <div className="msg-quem">{papel}</div>
      <div className="msg-texto">{texto}</div>
      {longa && (
        <button className="msg-mais" onClick={() => setInteira(!inteira)}>
          {inteira ? "mostrar menos" : `mostrar tudo (+${linhas.length - LINHAS_RESUMO} linhas)`}
        </button>
      )}
    </div>
  );
}

function CardAgente({ card }: { card: Card }) {
  const [texto, setTexto] = useState("");
  return (
    <div className="agente">
      <div className="agente-tarefa">
        <span className="dica">tarefa</span> {card.detalhe}
      </div>
      <pre className="agente-saida">{card.saida}</pre>
      <form
        className="agente-seguir"
        onSubmit={(e) => {
          e.preventDefault();
          if (!texto.trim()) return;
          void nucleo.iterarAgente(card.indice, texto.trim());
          setTexto("");
        }}
      >
        <input
          value={texto}
          onChange={(e) => setTexto(e.target.value)}
          placeholder={card.estado === "trabalhando" ? "Vai para a fila do agente" : "Responder ao agente"}
        />
        {card.consumo && <span className="dica mono">{card.consumo}</span>}
      </form>
    </div>
  );
}

function colunas(n: number): number {
  if (n <= 1) return 1;
  if (n <= 4) return 2;
  if (n <= 6) return 3;
  return 4;
}

export function App() {
  const [foto, setFoto] = useState<Foto | null>(null);
  const [erro, setErro] = useState<string | null>(null);
  const [vista, setVista] = useState<Vista>("workbench");
  const [paleta, setPaleta] = useState({ aberta: false, texto: "" });
  const [provedores, setProvedores] = useState(false);
  const [modelos, setModelos] = useState(false);
  const [manual, setManual] = useState(false);
  const [memoria, setMemoria] = useState(false);
  const [chatVisivel, setChatVisivel] = useState(true);
  const [entrada, setEntrada] = useState("");
  const entradaRef = useRef<HTMLTextAreaElement>(null);
  const fimChat = useRef<HTMLDivElement>(null);

  useEffect(() => {
    nucleo.estado().then(setFoto).catch((e) => setErro(String(e)));
    const soltarEstado = nucleo.aoMudarEstado(setFoto);
    const soltarPedido = nucleo.aoPedir((p) => {
      if (p === "focar-chat") entradaRef.current?.focus();
      if (p === "abrir-manual") setManual(true);
      if (p === "escolher-modelo") setModelos(true);
      if (p === "escolher-provedor") setProvedores(true);
    });
    return () => {
      void soltarEstado.then((f) => f());
      void soltarPedido.then((f) => f());
    };
  }, []);

  useEffect(() => {
    fimChat.current?.scrollIntoView({ block: "end" });
  }, [foto?.chat.length, foto?.chatParcial]);

  const inserirNoChat = (texto: string) => {
    setVista("workbench");
    setChatVisivel(true);
    setEntrada(texto);
    requestAnimationFrame(() => entradaRef.current?.focus());
  };

  const acao = (a: AcaoJanela) => {
    if (a === "memoria") setMemoria(true);
    if (a === "decisoes") setVista("decisoes");
    if (a === "manual") setManual(true);
    if (a === "workbench") setVista("workbench");
  };

  useEffect(() => {
    const tecla = (e: KeyboardEvent) => {
      const noTerminal = (e.target as HTMLElement | null)?.closest?.(".xterm") != null;
      const ws = foto?.workspaces[foto.workspace];
      const trata = (f: () => void) => {
        e.preventDefault();
        e.stopPropagation();
        f();
      };
      if (e.key === "F1") return trata(() => setManual((v) => !v));
      if (e.key === "F2") return trata(() => setVista((v) => (v === "decisoes" ? "workbench" : "decisoes")));
      if (e.key === "F4" || (e.ctrlKey && e.shiftKey && e.key.toLowerCase() === "w")) return trata(() => setMemoria((v) => !v));
      if (e.ctrlKey && e.shiftKey && e.key.toLowerCase() === "p") return trata(() => setPaleta({ aberta: true, texto: "" }));
      if (e.ctrlKey && !e.shiftKey && e.key.toLowerCase() === "k" && !noTerminal) return trata(() => setPaleta({ aberta: true, texto: "" }));
      if (e.key === "Escape" && (manual || memoria)) return trata(() => (setManual(false), setMemoria(false)));
      if (e.altKey && /^[1-8]$/.test(e.key) && ws) {
        const i = Number(e.key) - 1;
        if (i < ws.cards.length) return trata(() => void nucleo.focarCard(i));
      }
      if (e.altKey && (e.key === "ArrowUp" || e.key === "ArrowDown") && foto) {
        const n = foto.workspaces.length;
        const destino = (foto.workspace + (e.key === "ArrowUp" ? n - 1 : 1)) % n;
        return trata(() => void nucleo.trocarWorkspace(destino));
      }
      if (e.altKey && e.key.toLowerCase() === "b") return trata(() => setChatVisivel((v) => !v));
    };
    window.addEventListener("keydown", tecla, true);
    return () => window.removeEventListener("keydown", tecla, true);
  }, [foto, manual, memoria]);

  if (erro) return <div className="falha">Não consegui falar com o núcleo: {erro}</div>;
  if (!foto) return <div className="carregando">Abrindo o Orchestrator…</div>;

  const ws = foto.workspaces[foto.workspace];
  // Só o que espera você: o que o orquestrador está decidindo não pede nada.
  const nDecisoes = foto.decisoes.filter((d) => d.revisor === "dono").length;
  const cols = colunas(ws.cards.length);
  // Sobrou card numa linha incompleta: o último ocupa o espaço vazio.
  const sobra = ws.cards.length % cols;

  const enviar = () => {
    const texto = entrada.trim();
    if (!texto) return;
    setEntrada("");
    void nucleo.enviar(texto).catch((e) => setErro(String(e)));
  };

  return (
    <div className="app">
      <header className="topo">
        <div className="marca">
          <Marca />
          <span>Orchestrator</span>
        </div>
        <div className="trilha" title={ws.pasta}>
          {foto.projeto}
        </div>
        <button className="gatilho" onClick={() => setPaleta({ aberta: true, texto: "" })} title="Comandos (Ctrl+K)">
          <svg viewBox="0 0 16 16" aria-hidden="true">
            <circle cx="7" cy="7" r="4" />
            <path d="m10 10 3 3" />
          </svg>
          <span>Buscar ou rodar comando…</span>
          <kbd>Ctrl K</kbd>
        </button>
        <div className="topo-acoes">
          <button
            className={`botao ${nDecisoes ? "botao-alerta" : ""}`}
            onClick={() => setVista(vista === "decisoes" ? "workbench" : "decisoes")}
            title="Decisões pendentes (F2)"
          >
            {nDecisoes ? `${nDecisoes} ${nDecisoes === 1 ? "decisão" : "decisões"}` : "Decisões"}
          </button>
          <button className="botao" onClick={() => setMemoria(true)} title="Memória (F4)">
            Memória
          </button>
          <button
            className="botao"
            onClick={() => setVista(vista === "sandbox" ? "workbench" : "sandbox")}
            title="Tela virtual: o que a sandbox de teste está mostrando"
          >
            Tela virtual
          </button>
          <button className="botao botao-icone" onClick={() => setManual(true)} title="Manual (F1)" aria-label="Manual">
            ?
          </button>
        </div>
      </header>

      <div className="corpo">
        <aside className="lateral">
          <div className="rotulo">Projetos</div>
          {foto.projetos.map((p) => (
            <button key={p} className={`item ${p === foto.projeto ? "ativo" : ""}`} onClick={() => void nucleo.enviar(`/projeto ${p}`)}>
              {p}
            </button>
          ))}
          <div className="rotulo">Workspaces</div>
          {foto.workspaces.map((w, i) => (
            <button key={w.numero} className={`item ${i === foto.workspace ? "ativo" : ""}`} onClick={() => void nucleo.trocarWorkspace(i)} title="Alt+↑ / Alt+↓">
              <span className="mono dica">{w.numero}</span>
              <span className="item-nome">{w.cards.length ? `${w.cards.length} card${w.cards.length > 1 ? "s" : ""}` : "vazia"}</span>
            </button>
          ))}
          <div className="rotulo">Pasta desta workspace</div>
          <button className="item pasta" onClick={() => void nucleo.enviar("/pasta")} title={`${ws.pasta} — clique para escolher outra`}>
            {ws.pasta.split("/").filter(Boolean).pop() ?? ws.pasta}
            {!ws.pastaPropria && <span className="dica"> (do projeto)</span>}
          </button>
          <div className="lateral-fim">
            <button className="botao botao-largo" onClick={() => inserirNoChat("/cli ")}>
              Abrir uma CLI
            </button>
            <p className="dica">Ou peça no chat: “abra uma CLI chamada frontend”.</p>
          </div>
        </aside>

        {vista === "decisoes" ? (
          <Decisoes
            decisoes={foto.decisoes}
            aoResolver={(id, sim) => void nucleo.resolverDecisao(id, sim)}
            aoResponder={(id, resposta) => void nucleo.responderPergunta(id, resposta)}
            voltar={() => setVista("workbench")}
          />
        ) : vista === "sandbox" ? (
          <Sandbox voltar={() => setVista("workbench")} />
        ) : (
          <>
            {chatVisivel && (
              <section className="chat">
                <div className="chat-topo">
                  <span className="forte">Chat</span>
                  <button className="chip mono" onClick={() => setProvedores(true)} title="Trocar quem responde no chat">
                    {foto.provedor}
                    <span aria-hidden="true">▾</span>
                  </button>
                  <button className="chip mono" onClick={() => setModelos(true)} title="Modelo do chat">
                    {foto.modelo || "padrão"}
                    <span aria-hidden="true">▾</span>
                  </button>
                  <span className="chip">{foto.postura}</span>
                  <button className="botao-leve" onClick={() => setChatVisivel(false)} title="Esconder o chat (Alt+B)">
                    esconder
                  </button>
                </div>
                <div className="chat-log">
                  {foto.chat.length === 0 && (
                    <div className="vazio">
                      <strong>Converse com o orquestrador sobre o projeto.</strong>
                      <span>Ele abre CLIs reais, manda as tarefas e avisa quando terminam. Digite / para ver os comandos.</span>
                    </div>
                  )}
                  {foto.chat.map((l, i) => (
                    <Mensagem key={i} linha={l} />
                  ))}
                  {foto.chatOcupado && (
                    <div className="msg msg-orquestrador">
                      <div className="msg-quem">orquestrador · digitando</div>
                      {foto.chatPensando && <div className="msg-texto pensando">{foto.chatPensando}</div>}
                      {foto.chatParcial && <div className="msg-texto">{foto.chatParcial}</div>}
                    </div>
                  )}
                  <div ref={fimChat} />
                </div>
                <div className="chat-entrada">
                  {foto.fila && <div className="dica">Na fila: {foto.fila}</div>}
                  <textarea
                    ref={entradaRef}
                    value={entrada}
                    rows={2}
                    placeholder="Mensagem para o orquestrador, ou / para comandos"
                    onChange={(e) => {
                      const v = e.target.value;
                      if (v === "/") {
                        setEntrada("");
                        setPaleta({ aberta: true, texto: "/" });
                        return;
                      }
                      setEntrada(v);
                    }}
                    onKeyDown={(e) => {
                      if (e.key === "Enter" && !e.shiftKey) {
                        e.preventDefault();
                        enviar();
                      }
                    }}
                  />
                  <div className="chat-rodape">
                    <span className="dica">Enter envia · Shift+Enter quebra linha</span>
                    <button className="botao botao-primario" onClick={enviar} disabled={!entrada.trim()}>
                      Enviar
                    </button>
                  </div>
                </div>
              </section>
            )}

            <section className="grade-area">
              <div className="grade-topo">
                {foto.workspaces.map((w, i) => (
                  <button
                    key={w.numero}
                    className={`aba ${i === foto.workspace ? "ativa" : ""}`}
                    onClick={() => void nucleo.trocarWorkspace(i)}
                    title={`${w.cards.length} card${w.cards.length === 1 ? "" : "s"} · Alt+↑/↓ troca`}
                  >
                    Workspace {w.numero}
                    {w.cards.length > 0 && <span className="contador">{w.cards.length}</span>}
                  </button>
                ))}
                {!chatVisivel && (
                  <button className="botao-leve" onClick={() => setChatVisivel(true)}>
                    mostrar chat
                  </button>
                )}
              </div>
              {ws.cards.length === 0 ? (
                <div className="grade-vazia">
                  <div className="vazio">
                    <strong>Workspace vazia.</strong>
                    <span>
                      Peça no chat, por exemplo “abra uma CLI chamada frontend e mande criar o README”: o orquestrador abre a CLI real aqui, manda a
                      tarefa e avisa quando ela concluir.
                    </span>
                    <button className="botao" onClick={() => inserirNoChat("/cli ")}>
                      Abrir uma CLI eu mesmo
                    </button>
                  </div>
                </div>
              ) : (
                <div className="grade" style={{ gridTemplateColumns: `repeat(${cols}, minmax(0, 1fr))` }}>
                  {ws.cards.map((card) => {
                    const [glifo, rotulo] = ESTADO[card.estado];
                    const focado = card.indice === ws.focado;
                    return (
                      <article
                        key={`${foto.workspace}-${card.indice}-${card.nome}`}
                        className={`card ${focado ? "focado" : ""}`}
                        style={sobra && card.indice === ws.cards.length - 1 ? { gridColumn: `span ${cols - sobra + 1}` } : undefined}
                      >
                        <header className="card-topo" onMouseDown={() => void nucleo.focarCard(card.indice)}>
                          <span className="forte">{card.nome}</span>
                          <span className="chip mono">{TIPO[card.tipo]}</span>
                          <span className={`estado estado-${card.estado}`}>
                            {glifo} {rotulo}
                          </span>
                          {card.autopilot && <span className="dica">{card.autopilot}</span>}
                          <span className="espaco" />
                          {card.indice < 8 && <kbd title={`Alt+${card.indice + 1} foca este card`}>Alt {card.indice + 1}</kbd>}
                          <button className="botao-leve" onClick={() => void nucleo.fecharCard(card.indice)} title="Fechar o card (encerra a CLI)">
                            ✕
                          </button>
                        </header>
                        {card.tipo === "agente" ? (
                          <CardAgente card={card} />
                        ) : (
                          <Terminal indice={card.indice} focado={focado} aoFocar={() => void nucleo.focarCard(card.indice)} />
                        )}
                      </article>
                    );
                  })}
                </div>
              )}
            </section>
          </>
        )}
      </div>

      <footer className="rodape">
        <span className="rodape-status">{foto.status}</span>
        <span className="espaco" />
        {foto.avisos.length > 0 && (
          <button className="botao-leve aviso" onClick={() => setManual(true)} title={foto.avisos.join("\n")}>
            ⚠ {foto.avisos.length} {foto.avisos.length === 1 ? "aviso" : "avisos"}
          </button>
        )}
        {foto.consumo && (
          <span className="dica mono" title="Consumo do chat nesta sessão">
            {foto.consumo}
          </span>
        )}
        <span className="dica">postura: {foto.postura}</span>
        <button className="botao-leve" onClick={() => setManual(true)}>
          manual <kbd>F1</kbd>
        </button>
      </footer>

      <Paleta aberta={paleta.aberta} textoInicial={paleta.texto} fechar={() => setPaleta({ aberta: false, texto: "" })} aoInserir={inserirNoChat} aoAcao={acao} />
      <Provedores aberta={provedores} fechar={() => setProvedores(false)} />
      <Modelos aberta={modelos} fechar={() => setModelos(false)} foto={foto} />
      <Manual aberto={manual} fechar={() => setManual(false)} />
      <Memoria aberta={memoria} fechar={() => setMemoria(false)} projeto={foto.projeto} />
    </div>
  );
}
