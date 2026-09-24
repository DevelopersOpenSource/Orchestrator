import { useEffect, useRef, useState } from "react";
import { nucleo } from "../nucleo";

/**
 * A "tela virtual": o que a sandbox de teste do orquestrador (`ui_open`) está
 * mostrando agora, ao vivo. Não é um terminal — é a imagem que o navegador
 * dentro do container está renderizando, atualizada a cada ~1.5s enquanto
 * esta tela estiver aberta.
 *
 * `projeto` entra na chave do localStorage e no `key` do componente (posto
 * pelo App): o nome de sandbox lembrado é POR PROJETO — sem isso, trocar de
 * projeto continuava mostrando (ou tentando mostrar) a sandbox do projeto
 * anterior.
 */
export function Sandbox({ projeto, voltar }: { projeto: string; voltar: () => void }) {
  const chaveStorage = `orchestrator.sandboxVista.${projeto}`;
  const [nome, setNome] = useState(() => {
    try {
      return localStorage.getItem(chaveStorage) ?? "";
    } catch {
      return "";
    }
  });
  const [imagem, setImagem] = useState<string | null>(null);
  const [url, setUrl] = useState("");
  const [docker, setDocker] = useState(false);
  const [ocupado, setOcupado] = useState(false);
  const [resposta, setResposta] = useState("");

  const agir = async (f: () => Promise<string>) => {
    setOcupado(true);
    setResposta("");
    try {
      setResposta(await f());
    } catch (e) {
      setResposta(String(e));
    } finally {
      setOcupado(false);
    }
  };
  const [checou, setChecou] = useState(false);
  const alvo = useRef(nome);
  alvo.current = nome;

  useEffect(() => {
    try {
      localStorage.setItem(chaveStorage, nome);
    } catch {
      /* modo privado ou storage bloqueado — sem persistência, sem problema */
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [nome]);

  useEffect(() => {
    let vivo = true;
    const olhar = async () => {
      const alvoAgora = alvo.current.trim();
      if (!alvoAgora) {
        if (vivo) {
          setImagem(null);
          setChecou(true);
        }
        return;
      }
      try {
        const data = await nucleo.telaViva(alvoAgora);
        if (vivo) {
          setImagem(data);
          setChecou(true);
        }
      } catch {
        if (vivo) setChecou(true);
      }
    };
    void olhar();
    const id = window.setInterval(() => void olhar(), 1500);
    return () => {
      vivo = false;
      window.clearInterval(id);
    };
  }, []);

  return (
    <section className="sandbox-vista">
      <header className="decisoes-topo">
        <div>
          <h1>Tela virtual — {projeto}</h1>
          <p>
            O que a sandbox de teste está mostrando agora neste projeto — a das IAs (digite o nome dela) ou uma que você mesmo inicia
            aqui, rodando o projeto isolado da sua máquina.
          </p>
        </div>
        <button className="botao" onClick={voltar} title="Voltar ao workbench">
          Voltar
        </button>
      </header>
      <div className="sandbox-controles">
        <input
          className="sandbox-nome"
          value={nome}
          onChange={(e) => {
            setNome(e.target.value);
            setChecou(false);
          }}
          placeholder="nome da sandbox (ex.: teste-frontend)"
          spellCheck={false}
        />
        <input
          className="sandbox-nome"
          value={url}
          onChange={(e) => setUrl(e.target.value)}
          placeholder="endereço (ex.: localhost:3000) — vazio só sobe o container"
          spellCheck={false}
        />
        <label className="dica sandbox-docker" title="Permite docker/podman e compose dentro da sandbox">
          <input type="checkbox" checked={docker} onChange={(e) => setDocker(e.target.checked)} /> docker dentro
        </label>
        <button
          className="botao"
          disabled={ocupado || !nome.trim()}
          onClick={() => void agir(() => nucleo.iniciarSandbox(nome.trim(), url.trim(), docker))}
        >
          {ocupado ? "subindo…" : "Iniciar"}
        </button>
        <button className="botao-leve" disabled={ocupado || !nome.trim()} onClick={() => void agir(() => nucleo.pararSandbox(nome.trim()))}>
          Parar
        </button>
      </div>
      {resposta && <pre className="sandbox-resposta">{resposta}</pre>}
      <div className="sandbox-tela">
        {imagem ? (
          <img src={imagem} alt={`tela ao vivo da sandbox "${nome}"`} />
        ) : (
          <div className="vazio">
            <strong>{nome.trim() ? "Nenhuma sandbox de pé com esse nome." : "Digite o nome de uma sandbox aberta."}</strong>
            <span>
              {checou
                ? "Ela precisa estar aberta com ui_open ou ui_exec — peça ao orquestrador para abrir uma, ou confira o nome."
                : "Procurando…"}
            </span>
          </div>
        )}
      </div>
    </section>
  );
}
