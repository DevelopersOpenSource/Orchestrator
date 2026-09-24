import { useEffect, useState } from "react";
import { nucleo, type RemotoStatus } from "../nucleo";

/**
 * Acesso remoto: abre um túnel Cloudflare (HTTPS, TLS de verdade) para uma
 * página de controle do Orchestrator, protegida por uma senha conferida no
 * servidor. O servidor local escuta SÓ em 127.0.0.1 — nada fica exposto por
 * IP+porta na rede; a única entrada é o túnel, e ele só sobe quando você liga.
 */
export function Remoto({ aberta, fechar }: { aberta: boolean; fechar: () => void }) {
  const [status, setStatus] = useState<RemotoStatus | null>(null);
  const [senha, setSenha] = useState("");
  const [ocupado, setOcupado] = useState(false);
  const [aviso, setAviso] = useState("");
  const [copiado, setCopiado] = useState(false);

  const recarregar = () => nucleo.remotoStatus().then(setStatus).catch((e) => setAviso(String(e)));

  useEffect(() => {
    if (!aberta) return;
    setAviso("");
    setSenha("");
    void recarregar();
  }, [aberta]);

  if (!aberta) return null;

  const salvarSenha = async () => {
    setOcupado(true);
    setAviso("");
    try {
      await nucleo.remotoDefinirSenha(senha);
      setSenha("");
      setAviso("senha definida.");
      await recarregar();
    } catch (e) {
      setAviso(String(e));
    } finally {
      setOcupado(false);
    }
  };

  const ligar = async () => {
    setOcupado(true);
    setAviso("abrindo o túnel…");
    try {
      const url = await nucleo.remotoLigar();
      setAviso("");
      setStatus((s) => ({ senhaDefinida: s?.senhaDefinida ?? true, url }));
    } catch (e) {
      setAviso(String(e));
    } finally {
      setOcupado(false);
    }
  };

  const desligar = async () => {
    setOcupado(true);
    try {
      await nucleo.remotoDesligar();
      await recarregar();
    } catch (e) {
      setAviso(String(e));
    } finally {
      setOcupado(false);
    }
  };

  const copiar = (url: string) => {
    void navigator.clipboard?.writeText(url);
    setCopiado(true);
    setTimeout(() => setCopiado(false), 1500);
  };

  return (
    <div className="sobreposicao" onMouseDown={fechar}>
      <div className="manual remoto" role="dialog" aria-label="Acesso remoto" onMouseDown={(e) => e.stopPropagation()}>
        <header>
          <h2>Acesso remoto</h2>
          <button className="botao" onClick={fechar}>
            Fechar
          </button>
        </header>
        <div className="remoto-corpo">
          <p className="dica">
            Abre um túnel Cloudflare (HTTPS) para uma página de controle protegida por senha. O servidor escuta só em{" "}
            <code>127.0.0.1</code> — nada exposto na rede local; o túnel só existe enquanto você o mantém ligado.
          </p>

          <div className="remoto-bloco">
            <strong>1. Senha</strong>
            <p className="dica">Conferida no servidor a cada acesso. Mínimo 6 caracteres.</p>
            <div className="remoto-linha">
              <input
                type="password"
                placeholder={status?.senhaDefinida ? "trocar a senha…" : "definir a senha"}
                value={senha}
                onChange={(e) => setSenha(e.target.value)}
                onKeyDown={(e) => e.key === "Enter" && senha.length >= 6 && void salvarSenha()}
              />
              <button className="botao" disabled={ocupado || senha.length < 6} onClick={() => void salvarSenha()}>
                {status?.senhaDefinida ? "Trocar" : "Definir"}
              </button>
            </div>
            {status && <p className="dica">{status.senhaDefinida ? "✓ senha definida" : "nenhuma senha ainda"}</p>}
          </div>

          <div className="remoto-bloco">
            <strong>2. Túnel</strong>
            {status?.url ? (
              <>
                <div className="remoto-url">
                  <a href={status.url} target="_blank" rel="noreferrer">
                    {status.url}
                  </a>
                  <button className="botao-leve" onClick={() => copiar(status.url!)}>
                    {copiado ? "copiado" : "copiar"}
                  </button>
                </div>
                <button className="botao" disabled={ocupado} onClick={() => void desligar()}>
                  Desligar túnel
                </button>
              </>
            ) : (
              <>
                <p className="dica">Ligado, o app fica alcançável nessa URL. Desligado, some da internet.</p>
                <button className="botao" disabled={ocupado || !status?.senhaDefinida} onClick={() => void ligar()}>
                  {ocupado ? "abrindo…" : "Ligar túnel"}
                </button>
                {!status?.senhaDefinida && <p className="dica">defina a senha primeiro.</p>}
              </>
            )}
          </div>

          {aviso && <p className="dica">{aviso}</p>}
        </div>
      </div>
    </div>
  );
}
