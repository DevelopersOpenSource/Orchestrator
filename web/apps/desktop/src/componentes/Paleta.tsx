import { useEffect, useState } from "react";
import { Command } from "cmdk";
import { nucleo, type ItemPaleta } from "../nucleo";

export type AcaoJanela = "memoria" | "decisoes" | "manual" | "workbench";

const GLIFO = { pronto: "●", argumento: "○", indisponivel: "✖" } as const;
const ROTULO = { pronto: "pronto", argumento: "falta argumento", indisponivel: "indisponível" } as const;

const ACOES: Array<{ acao: AcaoJanela; nome: string; atalho: string }> = [
  { acao: "memoria", nome: "Abrir a memória", atalho: "F4" },
  { acao: "decisoes", nome: "Ver decisões pendentes", atalho: "F2" },
  { acao: "manual", nome: "Abrir o manual", atalho: "F1" },
];

/**
 * Os mesmos comandos `/` da TUI, com o estado de cada um: pronto, falta
 * argumento (qual) ou indisponível (por quê).
 */
export function Paleta({
  aberta,
  fechar,
  textoInicial,
  aoInserir,
  aoAcao,
}: {
  aberta: boolean;
  fechar: () => void;
  textoInicial: string;
  aoInserir: (texto: string) => void;
  aoAcao: (acao: AcaoJanela) => void;
}) {
  const [texto, setTexto] = useState(textoInicial);
  const [itens, setItens] = useState<ItemPaleta[]>([]);

  useEffect(() => {
    if (aberta) setTexto(textoInicial);
  }, [aberta, textoInicial]);

  useEffect(() => {
    if (!aberta) return;
    let vivo = true;
    const nome = texto.trim().split(/\s+/)[0] ?? "";
    void nucleo.paleta(nome || "/").then((r) => vivo && setItens(r));
    return () => {
      vivo = false;
    };
  }, [aberta, texto]);

  const escolher = (item: ItemPaleta) => {
    // `/provedor` sem nome já faz algo útil: abre a lista.
    const temArgumento = item.uso.includes(" ") && item.nome !== "/provedor";
    fechar();
    if (item.estado === "indisponivel") {
      aoInserir(`${item.nome} `);
    } else if (temArgumento) {
      aoInserir(`${item.nome} `);
    } else {
      void nucleo.enviar(item.nome);
    }
  };

  return (
    <Command.Dialog
      open={aberta}
      onOpenChange={(v) => !v && fechar()}
      label="Comandos"
      shouldFilter={false}
      className="paleta"
      overlayClassName="paleta-fundo"
      contentClassName="paleta-conteudo"
    >
      <Command.Input value={texto} onValueChange={setTexto} placeholder="Comando, ou o que você quer fazer" />
      <Command.List>
        <Command.Empty>Nenhum comando com esse nome. Esc fecha; F1 mostra o manual.</Command.Empty>
        <Command.Group heading="Comandos do chat">
          {itens.map((item) => (
            <Command.Item key={item.nome} value={item.nome} onSelect={() => escolher(item)} data-estado={item.estado}>
              <span className="p-glifo" aria-label={ROTULO[item.estado]}>
                {GLIFO[item.estado]}
              </span>
              <span className="p-uso">{item.uso}</span>
              <span className="p-sobre">{item.sobre}</span>
              {item.motivo && <span className="p-motivo">{item.motivo}</span>}
            </Command.Item>
          ))}
        </Command.Group>
        {!texto.startsWith("/") && (
          <Command.Group heading="Janela">
            {ACOES.map((a) => (
              <Command.Item
                key={a.acao}
                value={a.nome}
                onSelect={() => {
                  fechar();
                  aoAcao(a.acao);
                }}
              >
                <span className="p-glifo">→</span>
                <span className="p-uso">{a.nome}</span>
                <kbd>{a.atalho}</kbd>
              </Command.Item>
            ))}
          </Command.Group>
        )}
      </Command.List>
    </Command.Dialog>
  );
}
