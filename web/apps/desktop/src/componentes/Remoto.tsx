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
  const [totp, setTotp] = useState<{ otpauth: string; secret: string } | null>(null);
  const [codigo, setCodigo] = useState("");

  const recarregar = () => nucleo.remotoStatus().then(setStatus).catch((e) => setAviso(String(e)));

  useEffect(() => {
    if (!aberta) return;
    setAviso("");
    setSenha("");
    void recarregar();
  }, [aberta]);

  // Recarrega o status a cada 4s enquanto aberto (para ver acessos novos).
  // Precisa ficar ANTES de qualquer return — todos os hooks são chamados
  // incondicionalmente, senão o React quebra ("more hooks than previous").
  useEffect(() => {
    if (!aberta) return;
    const id = window.setInterval(() => void recarregar(), 4000);
    return () => window.clearInterval(id);
    // eslint-disable-next-line react-hooks/exhaustive-deps
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
      await nucleo.remotoLigar();
      setAviso("");
      await recarregar();
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

  const acao = async (f: () => Promise<unknown>, msg: string) => {
    setOcupado(true);
    setAviso("");
    try {
      await f();
      setAviso(msg);
      await recarregar();
    } catch (e) {
      setAviso(String(e));
    } finally {
      setOcupado(false);
    }
  };

  const iniciarTotp = async () => {
    setAviso("");
    try {
      setTotp(await nucleo.remotoTotpIniciar());
    } catch (e) {
      setAviso(String(e));
    }
  };

  const ativarTotp = async () => {
    setOcupado(true);
    setAviso("");
    try {
      await nucleo.remotoTotpAtivar(codigo.trim());
      setTotp(null);
      setCodigo("");
      setAviso("2FA ativado.");
      await recarregar();
    } catch (e) {
      setAviso(String(e));
    } finally {
      setOcupado(false);
    }
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
                <p className="dica">Link completo (guarde — o caminho é secreto; sem ele dá 404):</p>
                <div className="remoto-url">
                  <a href={status.url + status.caminho} target="_blank" rel="noreferrer">
                    {status.url + status.caminho}
                  </a>
                  <button className="botao-leve" onClick={() => copiar(status.url! + status.caminho)}>
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

          <div className="remoto-bloco">
            <strong>3. Verificação em duas etapas (2FA)</strong>
            {status?.totpAtivo ? (
              <>
                <p className="dica">✓ ativo — o login exige o código do seu autenticador (muda a cada 30s).</p>
                <button className="botao-leve" disabled={ocupado} onClick={() => void acao(nucleo.remotoTotpDesativar, "2FA desativado.")}>
                  Desativar 2FA
                </button>
              </>
            ) : totp ? (
              <>
                <p className="dica">
                  No app autenticador (Google Authenticator, Aegis…): escaneie ou digite o segredo abaixo, depois confirme com o
                  código que aparecer.
                </p>
                <div className="remoto-url">
                  <span className="mono" style={{ wordBreak: "break-all" }}>{totp.secret}</span>
                  <button className="botao-leve" onClick={() => copiar(totp.secret)}>
                    {copiado ? "copiado" : "copiar"}
                  </button>
                </div>
                <p className="dica" style={{ wordBreak: "break-all" }}>{totp.otpauth}</p>
                <div className="remoto-linha">
                  <input inputMode="numeric" placeholder="código atual (6 dígitos)" value={codigo} onChange={(e) => setCodigo(e.target.value)} />
                  <button className="botao" disabled={ocupado || codigo.trim().length < 6} onClick={() => void ativarTotp()}>
                    Confirmar
                  </button>
                </div>
              </>
            ) : (
              <>
                <p className="dica">Mesmo com o link e a senha, sem o código do seu celular ninguém entra.</p>
                <button className="botao-leve" onClick={() => void iniciarTotp()}>
                  Ativar 2FA
                </button>
              </>
            )}
          </div>

          <div className="remoto-bloco">
            <strong>Se o link vazar</strong>
            <p className="dica">Gera um link novo (o antigo morre na hora) e derruba qualquer sessão aberta.</p>
            <div className="remoto-linha">
              <button className="botao-leve" disabled={ocupado} onClick={() => void acao(nucleo.remotoRegenerarToken, "link novo gerado; o antigo foi invalidado.")}>
                Gerar link novo
              </button>
              <button className="botao-leve" disabled={ocupado} onClick={() => void acao(nucleo.remotoRevogarSessoes, "sessões derrubadas.")}>
                Revogar sessões
              </button>
            </div>
          </div>

          {status && status.acessos.length > 0 && (
            <div className="remoto-bloco">
              <strong>Acessos recentes</strong>
              <ul className="remoto-acessos">
                {status.acessos.slice(0, 12).map((a, idx) => (
                  <li key={idx} className={a.ok ? "ok" : "falha"}>
                    <span>{a.ok ? "entrou" : "senha errada"}</span>
                    <span className="mono">{a.ip}</span>
                    <span className="dica">{new Date(a.ms).toLocaleString()}</span>
                  </li>
                ))}
              </ul>
            </div>
          )}

          {aviso && <p className="dica">{aviso}</p>}
        </div>
      </div>
    </div>
  );
}
