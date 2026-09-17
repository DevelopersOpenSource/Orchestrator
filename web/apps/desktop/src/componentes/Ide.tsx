import { useEffect, useMemo, useRef, useState } from "react";
import { EditorState } from "@codemirror/state";
import { EditorView, keymap, lineNumbers, highlightActiveLine } from "@codemirror/view";
import { defaultKeymap, history, historyKeymap, indentWithTab } from "@codemirror/commands";
import { search, searchKeymap } from "@codemirror/search";
import { javascript } from "@codemirror/lang-javascript";
import { python } from "@codemirror/lang-python";
import { rust } from "@codemirror/lang-rust";
import { json } from "@codemirror/lang-json";
import { css } from "@codemirror/lang-css";
import { html } from "@codemirror/lang-html";
import { markdown } from "@codemirror/lang-markdown";
import { oneDark } from "@codemirror/theme-one-dark";
import { nucleo, type EntradaArquivo } from "../nucleo";

/**
 * A IDE do projeto: uma árvore de arquivos à esquerda e um editor de texto
 * (CodeMirror 6, com destaque para as linguagens mais comuns) à direita.
 * Não é o VSCode — é o suficiente para abrir, editar e salvar um arquivo do
 * projeto sem sair do Orchestrator. Sem LSP, sem git, sem extensões.
 */
export function Ide({ voltar }: { voltar: () => void }) {
  const [arvore, setArvore] = useState<Record<string, EntradaArquivo[]>>({});
  const [abertas, setAbertas] = useState<Set<string>>(new Set());
  const [ativo, setAtivo] = useState<string | null>(null);
  const [conteudo, setConteudo] = useState("");
  const [sujo, setSujo] = useState(false);
  const [erro, setErro] = useState<string | null>(null);
  const [salvando, setSalvando] = useState(false);
  const editorRef = useRef<HTMLDivElement | null>(null);
  const viewRef = useRef<EditorView | null>(null);
  const conteudoRef = useRef(conteudo);
  conteudoRef.current = conteudo;

  const carregarPasta = async (caminho: string) => {
    try {
      const entradas = await nucleo.ideListar(caminho);
      setArvore((a) => ({ ...a, [caminho]: entradas }));
    } catch (e) {
      setErro(String(e));
    }
  };

  useEffect(() => {
    void carregarPasta("");
  }, []);

  const alternarPasta = (caminho: string) => {
    setAbertas((a) => {
      const nova = new Set(a);
      if (nova.has(caminho)) nova.delete(caminho);
      else {
        nova.add(caminho);
        if (!arvore[caminho]) void carregarPasta(caminho);
      }
      return nova;
    });
  };

  const abrir = async (caminho: string) => {
    if (ativo && sujo && !window.confirm(`Descartar as mudanças em ${ativo}?`)) return;
    try {
      setErro(null);
      const texto = await nucleo.ideLer(caminho);
      setAtivo(caminho);
      setConteudo(texto);
      setSujo(false);
    } catch (e) {
      setErro(String(e));
    }
  };

  const salvar = async () => {
    if (!ativo) return;
    setSalvando(true);
    try {
      await nucleo.ideSalvar(ativo, conteudoRef.current);
      setSujo(false);
    } catch (e) {
      setErro(String(e));
    } finally {
      setSalvando(false);
    }
  };

  const extensoes = useMemo(() => {
    const linguagem = ativo ? linguagemPara(ativo) : null;
    const base = [
      lineNumbers(),
      highlightActiveLine(),
      history(),
      search(),
      keymap.of([...defaultKeymap, ...historyKeymap, ...searchKeymap, indentWithTab]),
      oneDark,
      EditorView.updateListener.of((v) => {
        if (v.docChanged) {
          conteudoRef.current = v.state.doc.toString();
          setConteudo(conteudoRef.current);
          setSujo(true);
        }
      }),
      EditorView.theme({ "&": { height: "100%", fontSize: "13px" }, ".cm-scroller": { fontFamily: "var(--fonte-mono, monospace)" } }),
    ];
    return linguagem ? [...base, linguagem] : base;
  }, [ativo]);

  // Salvar com Ctrl+S / Cmd+S sem sair do editor.
  useEffect(() => {
    const ouvinte = (e: KeyboardEvent) => {
      if ((e.ctrlKey || e.metaKey) && e.key === "s" && ativo) {
        e.preventDefault();
        void salvar();
      }
    };
    window.addEventListener("keydown", ouvinte);
    return () => window.removeEventListener("keydown", ouvinte);
  }, [ativo]);

  // Recria a instância do CodeMirror a cada arquivo aberto (linguagem muda).
  useEffect(() => {
    if (!editorRef.current) return;
    viewRef.current?.destroy();
    viewRef.current = new EditorView({
      state: EditorState.create({ doc: conteudo, extensions: extensoes }),
      parent: editorRef.current,
    });
    return () => {
      viewRef.current?.destroy();
      viewRef.current = null;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [ativo]);

  return (
    <section className="ide-vista">
      <header className="decisoes-topo">
        <div>
          <h1>IDE</h1>
          <p>Abra e edite arquivos do projeto direto aqui — sem LSP, sem git, só o essencial.</p>
        </div>
        <div className="ide-acoes-topo">
          {ativo && (
            <button className="botao" onClick={() => void salvar()} disabled={!sujo || salvando} title="Salvar (Ctrl+S)">
              {salvando ? "salvando…" : sujo ? "Salvar •" : "Salvo"}
            </button>
          )}
          <button className="botao" onClick={voltar} title="Voltar ao workbench">
            Voltar
          </button>
        </div>
      </header>
      {erro && <div className="vazio erro">{erro}</div>}
      <div className="ide-corpo">
        <nav className="ide-arvore">
          <Pasta caminho="" entradas={arvore[""] ?? []} arvore={arvore} abertas={abertas} ativo={ativo} onPasta={alternarPasta} onArquivo={(c) => void abrir(c)} />
        </nav>
        <div className="ide-editor">
          {ativo ? (
            <div ref={editorRef} className="ide-cm" />
          ) : (
            <div className="vazio">
              <strong>Nenhum arquivo aberto.</strong>
              <span>Escolha um arquivo na árvore à esquerda.</span>
            </div>
          )}
        </div>
      </div>
    </section>
  );
}

function Pasta({
  caminho,
  entradas,
  arvore,
  abertas,
  ativo,
  onPasta,
  onArquivo,
}: {
  caminho: string;
  entradas: EntradaArquivo[];
  arvore: Record<string, EntradaArquivo[]>;
  abertas: Set<string>;
  ativo: string | null;
  onPasta: (c: string) => void;
  onArquivo: (c: string) => void;
}) {
  return (
    <ul className="ide-lista">
      {entradas.map((e) => (
        <li key={e.caminho}>
          {e.pasta ? (
            <>
              <button className="ide-item ide-pasta" onClick={() => onPasta(e.caminho)}>
                <span aria-hidden="true">{abertas.has(e.caminho) ? "▾" : "▸"}</span> {e.nome}
              </button>
              {abertas.has(e.caminho) && (
                <div className="ide-filhos">
                  <Pasta caminho={e.caminho} entradas={arvore[e.caminho] ?? []} arvore={arvore} abertas={abertas} ativo={ativo} onPasta={onPasta} onArquivo={onArquivo} />
                </div>
              )}
            </>
          ) : (
            <button className={`ide-item ide-arquivo ${ativo === e.caminho ? "ide-item-ativo" : ""}`} onClick={() => onArquivo(e.caminho)}>
              {e.nome}
            </button>
          )}
        </li>
      ))}
    </ul>
  );
}

function linguagemPara(caminho: string) {
  const ext = caminho.split(".").pop()?.toLowerCase() ?? "";
  switch (ext) {
    case "ts":
    case "tsx":
    case "js":
    case "jsx":
    case "mjs":
      return javascript({ jsx: true, typescript: ext.startsWith("ts") });
    case "py":
      return python();
    case "rs":
      return rust();
    case "json":
      return json();
    case "css":
      return css();
    case "html":
      return html();
    case "md":
      return markdown();
    default:
      return null;
  }
}
