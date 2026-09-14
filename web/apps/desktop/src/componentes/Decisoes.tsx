import { useState } from "react";
import type { Decisao } from "../nucleo";

/**
 * O que precisa do dono: pedidos que o orquestrador passou para ele e
 * perguntas com alternativas. À parte, sem pedir nada, o que o orquestrador
 * está decidindo sozinho no modo autônomo.
 */
export function Decisoes({
  decisoes,
  aoResolver,
  aoResponder,
  voltar,
}: {
  decisoes: Decisao[];
  aoResolver: (id: string, aprovar: boolean) => void;
  aoResponder: (id: string, resposta: string) => void;
  voltar: () => void;
}) {
  const suas = decisoes.filter((d) => d.revisor === "dono");
  const doOrquestrador = decisoes.filter((d) => d.revisor === "orquestrador");
  return (
    <section className="decisoes">
      <header className="decisoes-topo">
        <div>
          <h1>Decisões</h1>
          <p>No modo autônomo o orquestrador decide o que as CLIs pedem. Aqui fica só o que precisa de você.</p>
        </div>
        <button className="botao" onClick={voltar} title="Voltar ao workbench (F2)">
          Voltar
        </button>
      </header>
      {suas.length === 0 ? (
        <div className="vazio">
          <strong>Nada esperando você.</strong>
          <span>
            Quando algo for crítico demais, fugir do que você pediu, ou o orquestrador precisar de uma escolha sua, aparece aqui e no
            chat.
          </span>
        </div>
      ) : (
        <ul className="decisoes-lista">
          {suas.map((d) =>
            d.tipo === "pergunta" ? (
              <Pergunta key={d.id} d={d} aoResponder={aoResponder} />
            ) : (
              <Acao key={d.id} d={d} aoResolver={aoResolver} />
            ),
          )}
        </ul>
      )}
      {doOrquestrador.length > 0 && (
        <div className="decisoes-orq">
          <div className="rotulo">O orquestrador está decidindo</div>
          <ul className="decisoes-lista">
            {doOrquestrador.map((d) => (
              <li key={d.id} className="decisao decisao-orq">
                <div className="decisao-cabeca">
                  <span className="selo">com o orquestrador</span>
                  <span className="decisao-onde">
                    {d.pedidoPor ? `${d.pedidoPor} · ` : ""}
                    {d.criada}
                  </span>
                </div>
                <p className="decisao-resumo">{d.resumo}</p>
                <div className="decisao-acoes">
                  <button className="botao-leve" onClick={() => aoResolver(d.id, false)}>
                    Negar eu mesmo
                  </button>
                  <button className="botao-leve" onClick={() => aoResolver(d.id, true)}>
                    Aprovar eu mesmo
                  </button>
                </div>
              </li>
            ))}
          </ul>
        </div>
      )}
    </section>
  );
}

function Acao({ d, aoResolver }: { d: Decisao; aoResolver: (id: string, aprovar: boolean) => void }) {
  return (
    <li className="decisao">
      <div className="decisao-cabeca">
        <span className="selo selo-av">○ aguardando você</span>
        <span className="decisao-onde">
          {d.pedidoPor ? `${d.pedidoPor} · ` : ""}
          {d.projeto} · {d.criada}
        </span>
      </div>
      <p className="decisao-resumo">{d.resumo}</p>
      {d.nota && <p className="decisao-nota">O orquestrador passou para você: {d.nota}</p>}
      <div className="decisao-acoes">
        <button className="botao" onClick={() => aoResolver(d.id, false)}>
          Negar
        </button>
        <button className="botao botao-primario" onClick={() => aoResolver(d.id, true)}>
          Aprovar
        </button>
      </div>
    </li>
  );
}

function Pergunta({ d, aoResponder }: { d: Decisao; aoResponder: (id: string, resposta: string) => void }) {
  const [marcadas, setMarcadas] = useState<string[]>([]);
  const [livre, setLivre] = useState("");
  const alternar = (opcao: string) =>
    setMarcadas((m) => (d.multipla ? (m.includes(opcao) ? m.filter((x) => x !== opcao) : [...m, opcao]) : [opcao]));
  const resposta = [...marcadas, ...(livre.trim() ? [livre.trim()] : [])].join(", ");
  return (
    <li className="decisao">
      <div className="decisao-cabeca">
        <span className="selo selo-av">? pergunta para você</span>
        <span className="decisao-onde">
          {d.projeto} · {d.criada}
        </span>
      </div>
      <p className="decisao-resumo">{d.resumo}</p>
      {d.opcoes.length > 0 && (
        <div className="pergunta-opcoes" role={d.multipla ? "group" : "radiogroup"}>
          {d.opcoes.map((opcao) => (
            <label key={opcao} className="pergunta-opcao">
              <input
                type={d.multipla ? "checkbox" : "radio"}
                name={`pergunta-${d.id}`}
                checked={marcadas.includes(opcao)}
                onChange={() => alternar(opcao)}
              />
              <span>{opcao}</span>
            </label>
          ))}
          {d.multipla && <span className="dica">Pode marcar mais de uma.</span>}
        </div>
      )}
      <input
        className="pergunta-livre"
        value={livre}
        onChange={(e) => setLivre(e.target.value)}
        placeholder={d.opcoes.length ? "Outra resposta (opcional)" : "Sua resposta"}
      />
      <div className="decisao-acoes">
        <button className="botao botao-primario" disabled={!resposta} onClick={() => aoResponder(d.id, resposta)}>
          Responder
        </button>
      </div>
    </li>
  );
}
