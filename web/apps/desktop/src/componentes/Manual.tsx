import { useEffect, useState } from "react";
import { nucleo, type ItemPaleta } from "../nucleo";

const ATALHOS: Array<[string, string]> = [
  ["Ctrl+K ou Ctrl+Shift+P", "comandos (fora do terminal: Ctrl+K)"],
  ["/ no chat", "abre os comandos já filtrando"],
  ["F1", "este manual"],
  ["F2", "decisões pendentes"],
  ["F4 ou Ctrl+Shift+W", "memória"],
  ["Alt+1 … Alt+8", "vai para o card N"],
  ["Alt+↑ / Alt+↓", "workspace anterior / próxima (o chat vai junto)"],
  ["Alt+B", "esconde ou mostra o chat"],
  ["Esc", "fecha o que estiver aberto por cima"],
];

export function Manual({ aberto, fechar }: { aberto: boolean; fechar: () => void }) {
  const [comandos, setComandos] = useState<ItemPaleta[]>([]);
  useEffect(() => {
    if (aberto) void nucleo.paleta("/").then(setComandos);
  }, [aberto]);
  if (!aberto) return null;
  return (
    <div className="sobreposicao" onMouseDown={fechar}>
      <div className="manual" role="dialog" aria-label="Manual" onMouseDown={(e) => e.stopPropagation()}>
        <header>
          <h2>Manual</h2>
          <button className="botao" onClick={fechar}>
            Fechar
          </button>
        </header>
        <div className="manual-colunas">
          <section>
            <h3>Atalhos</h3>
            <p className="dica">Teclas com Ctrl são das CLIs quando o terminal está em foco; a janela só usa Alt e F.</p>
            <dl>
              {ATALHOS.map(([tecla, desc]) => (
                <div key={tecla}>
                  <dt>
                    <kbd>{tecla}</kbd>
                  </dt>
                  <dd>{desc}</dd>
                </div>
              ))}
            </dl>
          </section>
          <section>
            <h3>Comandos do chat</h3>
            <dl>
              {comandos.map((c) => (
                <div key={c.nome}>
                  <dt>
                    <code>{c.uso}</code>
                  </dt>
                  <dd>{c.sobre}</dd>
                </div>
              ))}
            </dl>
          </section>
        </div>
      </div>
    </div>
  );
}
