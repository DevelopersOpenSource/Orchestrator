import { useState } from "react";
import { nucleo, type Foto } from "../nucleo";

const MODOS: { id: string; titulo: string; risco: string; texto: string }[] = [
  {
    id: "autonomo",
    titulo: "Autônomo",
    risco: "mais seguro",
    texto:
      "Sem você por perto: a IA reduz a própria permissão. O catastrófico é bloqueado e TUDO que é arriscado espera a sua decisão — nada arriscado é auto-aprovado.",
  },
  {
    id: "padrao",
    titulo: "Padrão",
    risco: "equilibrado",
    texto:
      "O catastrófico é bloqueado, o arriscado pausa e você responde. O orquestrador decide sozinho os pedidos de CLI de que tem certeza. Bom para o dia a dia com você por perto.",
  },
  {
    id: "bypass",
    titulo: "Bypass",
    risco: "você assume o risco",
    texto:
      "A trava libera TUDO — nem bloqueio, nem pausa (como um sudo). Use só com você no controle, acompanhando. A IA nunca liga isto sozinha; é uma escolha sua.",
  },
];

/**
 * Configurações do app. Por ora, o nível de permissão do orquestrador — o
 * quanto ele pode agir sozinho (terminal, ssh, comandos) antes de parar para
 * te perguntar.
 */
export function Configuracoes({ aberta, fechar, foto }: { aberta: boolean; fechar: () => void; foto: Foto }) {
  const [aviso, setAviso] = useState("");
  if (!aberta) return null;

  const trocar = async (id: string) => {
    setAviso("");
    try {
      setAviso(await nucleo.definirPermModo(id));
    } catch (e) {
      setAviso(String(e));
    }
  };

  return (
    <div className="sobreposicao" onMouseDown={fechar}>
      <div className="manual config" role="dialog" aria-label="Configurações" onMouseDown={(e) => e.stopPropagation()}>
        <header>
          <h2>Configurações</h2>
          <button className="botao" onClick={fechar}>
            Fechar
          </button>
        </header>
        <div className="config-corpo">
          <strong>Nível de permissão do orquestrador</strong>
          <p className="dica">
            Quanto o orquestrador pode fazer sozinho (rodar comando, terminal, SSH) antes de te perguntar. A regra vale para a IA;
            você continua no controle.
          </p>
          <div className="config-modos">
            {MODOS.map((m) => {
              const ativo = foto.permModo === m.id;
              return (
                <button
                  key={m.id}
                  className={`config-modo ${ativo ? "ativo" : ""} ${m.id === "bypass" ? "perigo" : ""}`}
                  onClick={() => void trocar(m.id)}
                >
                  <span className="config-modo-topo">
                    <strong>{m.titulo}</strong>
                    <span className="dica">{m.risco}</span>
                    {ativo && <span className="config-atual">● atual</span>}
                  </span>
                  <span className="dica">{m.texto}</span>
                </button>
              );
            })}
          </div>
          {aviso && <p className="dica">{aviso}</p>}
          <p className="dica">
            Mesmo no autônomo/padrão, as regras de segurança do projeto (bloquear <code>rm -rf</code>, <code>mkfs</code>, pausar
            deploy/força-bruta) continuam valendo. O bypass é o único que passa por cima delas.
          </p>
        </div>
      </div>
    </div>
  );
}
