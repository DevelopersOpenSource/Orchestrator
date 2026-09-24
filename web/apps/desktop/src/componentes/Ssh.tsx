import { useEffect, useState } from "react";
import { open } from "@tauri-apps/plugin-dialog";
import { nucleo, type SshHost } from "../nucleo";

const VAZIO: SshHost = { nome: "", host: "", usuario: "root", porta: 22, chave: "" };

/**
 * Servidores SSH deste projeto. A IA usa com `ssh_exec` (sem pedir senha,
 * com a sua chave) e você abre um terminal em cada um. A chave privada nunca
 * é copiada — só o caminho dela fica guardado.
 */
export function Ssh({ aberta, fechar, projeto }: { aberta: boolean; fechar: () => void; projeto: string }) {
  const [hosts, setHosts] = useState<SshHost[]>([]);
  const [novo, setNovo] = useState<SshHost>(VAZIO);
  const [aviso, setAviso] = useState("");

  useEffect(() => {
    if (!aberta) return;
    setAviso("");
    setNovo(VAZIO);
    void nucleo.sshListar().then(setHosts).catch((e) => setAviso(String(e)));
  }, [aberta, projeto]);

  if (!aberta) return null;

  const salvar = async (lista: SshHost[]) => {
    try {
      setAviso(await nucleo.sshSalvar(lista));
      setHosts(lista);
    } catch (e) {
      setAviso(String(e));
    }
  };

  const adicionar = () => {
    if (!novo.nome.trim() || !novo.host.trim()) {
      setAviso("preencha pelo menos o nome e o endereço");
      return;
    }
    const lista = [...hosts.filter((h) => h.nome !== novo.nome.trim()), { ...novo, nome: novo.nome.trim() }];
    void salvar(lista);
    setNovo(VAZIO);
  };

  const escolherChave = async () => {
    const r = await open({ title: "Chave privada SSH", multiple: false, directory: false });
    if (typeof r === "string") setNovo((n) => ({ ...n, chave: r }));
  };

  return (
    <div className="sobreposicao" onMouseDown={fechar}>
      <div className="manual ssh" role="dialog" aria-label="Conexões SSH" onMouseDown={(e) => e.stopPropagation()}>
        <header>
          <h2>Conexões SSH — {projeto}</h2>
          <button className="botao" onClick={fechar}>
            Fechar
          </button>
        </header>
        <div className="ssh-corpo">
          <p className="dica">
            A IA deste projeto usa estes servidores com a sua chave, sem pedir senha (tool <code>ssh_exec</code>). Comandos perigosos
            continuam passando pelas regras de segurança.
          </p>
          {hosts.length === 0 ? (
            <div className="vazio">
              <strong>Nenhum servidor ainda.</strong>
              <span>Cadastre abaixo — usuário root é o padrão.</span>
            </div>
          ) : (
            <ul className="ssh-lista">
              {hosts.map((h) => (
                <li key={h.nome}>
                  <span className="mono">
                    <strong>{h.nome}</strong> {h.usuario}@{h.host}:{h.porta}
                  </span>
                  <span className="dica mono">{h.chave || "sem chave (usa o ssh-agent)"}</span>
                  <span className="ssh-acoes">
                    <button className="botao-leve" onClick={() => void nucleo.abrirSsh(h.nome).then(setAviso)}>
                      Terminal
                    </button>
                    <button className="botao-leve" onClick={() => void salvar(hosts.filter((x) => x.nome !== h.nome))}>
                      Remover
                    </button>
                  </span>
                </li>
              ))}
            </ul>
          )}
          <div className="ssh-form">
            <input placeholder="nome (ex.: vps-producao)" value={novo.nome} onChange={(e) => setNovo({ ...novo, nome: e.target.value })} />
            <input placeholder="endereço (IP ou domínio)" value={novo.host} onChange={(e) => setNovo({ ...novo, host: e.target.value })} />
            <input placeholder="usuário" value={novo.usuario} onChange={(e) => setNovo({ ...novo, usuario: e.target.value })} />
            <input
              placeholder="porta"
              inputMode="numeric"
              value={novo.porta}
              onChange={(e) => setNovo({ ...novo, porta: Number(e.target.value.replace(/\D/g, "")) || 22 })}
            />
            <input placeholder="caminho da chave privada" value={novo.chave} onChange={(e) => setNovo({ ...novo, chave: e.target.value })} />
            <button className="botao-leve" onClick={() => void escolherChave()}>
              Escolher…
            </button>
            <button className="botao" onClick={adicionar}>
              Adicionar
            </button>
          </div>
          {aviso && <p className="dica">{aviso}</p>}
        </div>
      </div>
    </div>
  );
}
