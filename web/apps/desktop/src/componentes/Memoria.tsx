import { useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { createGlobe, KIND_LABELS, type Globe, type GraphData, type Kind } from "@orchestrator/globe";
import { nucleo } from "../nucleo";

interface Achado {
  id: string;
  title: string;
  body: string;
  kind: Kind;
  project: string;
  scope: string;
  origin: string;
  author: string;
  score: number;
}

/** A memória do projeto ativo: busca por sentido e o mesmo globo da página. */
export function Memoria({ aberta, fechar, projeto }: { aberta: boolean; fechar: () => void; projeto: string }) {
  const [base, setBase] = useState<string | null>(null);
  const [q, setQ] = useState("");
  const [achados, setAchados] = useState<Achado[] | null>(null);
  const [aberto, setAberto] = useState<string | null>(null);
  const [erro, setErro] = useState<string | null>(null);
  const [info, setInfo] = useState("");
  const palco = useRef<HTMLDivElement>(null);
  const globo = useRef<Globe | null>(null);

  useEffect(() => {
    if (aberta && !base) void nucleo.memoriaApi().then(setBase);
  }, [aberta, base]);

  useEffect(() => {
    const el = palco.current;
    if (!aberta || !base || !el) return;
    const g = createGlobe(el, { onSelect: (n) => n?.type === "memory" && setAberto(n.id) });
    globo.current = g;
    setErro(null);
    fetch(`${base}/api/graph?project=${encodeURIComponent(projeto)}`)
      .then((r) => r.json())
      .then((d: GraphData) => g.setData(d))
      .catch(() => setErro(`O serviço da memória não respondeu em ${base}. Ele sobe sozinho com a primeira mensagem do chat.`));
    const ajustar = () => {
      const r = el.getBoundingClientRect();
      if (r.width && r.height) g.resize(r.width, r.height);
    };
    const ro = new ResizeObserver(ajustar);
    ro.observe(el);
    ajustar();
    return () => {
      ro.disconnect();
      g.dispose();
      globo.current = null;
    };
  }, [aberta, base, projeto]);

  useEffect(() => {
    if (!aberta || !base) return;
    const termo = q.trim();
    if (!termo) {
      setAchados(null);
      globo.current?.highlight([]);
      return;
    }
    const controle = new AbortController();
    const espera = window.setTimeout(() => void buscar(termo, false, controle.signal), 180);
    return () => {
      window.clearTimeout(espera);
      controle.abort();
    };
  }, [q, aberta, base, projeto]);

  async function buscar(termo: string, rerank: boolean, signal?: AbortSignal) {
    if (!base) return;
    try {
      const url = `${base}/api/search?q=${encodeURIComponent(termo)}&project=${encodeURIComponent(projeto)}&rerank=${rerank}`;
      const r = await fetch(url, { signal }).then((x) => x.json());
      setAchados(r.hits ?? []);
      setInfo(`${r.took_ms} ms · ${r.reranked ? "reranker" : r.semantic ? "prévia por sentido" : "por palavras"}`);
      globo.current?.highlight((r.hits ?? []).map((h: Achado) => h.id));
      if (rerank && r.hits?.[0]) globo.current?.focus(r.hits[0].id);
    } catch (e) {
      if ((e as Error).name !== "AbortError") setErro(`A busca falhou: ${String(e)}`);
    }
  }

  if (!aberta) return null;
  const maior = Math.max(0.0001, ...(achados ?? []).map((a) => a.score));
  return (
    <div className="sobreposicao" onMouseDown={fechar}>
      <div className="memoria" role="dialog" aria-label="Memória" onMouseDown={(e) => e.stopPropagation()}>
        <header className="memoria-topo">
          <h2>Memória</h2>
          <span className="memoria-projeto">{projeto}</span>
          <form
            className="memoria-busca"
            onSubmit={(e) => {
              e.preventDefault();
              if (q.trim()) void buscar(q.trim(), true);
            }}
          >
            <input autoFocus value={q} onChange={(e) => setQ(e.target.value)} placeholder="Pergunte à memória deste projeto" />
          </form>
          {base && (
            <button className="botao" onClick={() => void invoke("plugin:opener|open_url", { url: base })} title={base}>
              Página completa
            </button>
          )}
          <button className="botao" onClick={fechar} title="Fechar (Esc, F4)">
            Fechar
          </button>
        </header>
        <div className="memoria-corpo">
          <div className="memoria-palco" ref={palco} />
          <aside className="memoria-lista">
            {erro && <div className="vazio erro">{erro}</div>}
            {achados === null ? (
              <p className="dica">Digite para ver os parecidos pelo sentido; Enter confirma com o reranker. Clique numa bolinha do globo para abrir.</p>
            ) : achados.length === 0 ? (
              <div className="vazio">
                <strong>Nada relevante.</strong>
                <span>A memória só mostra o que tem relação de verdade.</span>
              </div>
            ) : (
              <>
                <p className="dica">{info}</p>
                <ol>
                  {achados.map((a) => (
                    <li key={a.id}>
                      <button
                        className="resultado"
                        aria-expanded={aberto === a.id}
                        onClick={() => {
                          setAberto(aberto === a.id ? null : a.id);
                          globo.current?.focus(a.id);
                        }}
                      >
                        <span className="r-titulo">{a.title}</span>
                        <span className="r-linha">
                          <span className="r-barra">
                            <span style={{ width: `${Math.round((a.score / maior) * 100)}%` }} />
                          </span>
                          <span className={`ponto k-${a.kind}`} />
                          {KIND_LABELS[a.kind] ?? a.kind}
                          <span className="r-onde">
                            {a.scope === "global" ? "todo projeto" : a.project} · {a.origin === "agent" ? `IA: ${a.author}` : "dono"}
                          </span>
                        </span>
                        {aberto === a.id && <span className="r-corpo">{a.body || "(sem texto)"}</span>}
                      </button>
                    </li>
                  ))}
                </ol>
              </>
            )}
          </aside>
        </div>
      </div>
    </div>
  );
}
