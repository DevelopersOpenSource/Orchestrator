import { useEffect, useState } from "react";
import { Command } from "cmdk";
import { nucleo, type ItemProvedor } from "../nucleo";

const GLIFO = { pronto: "●", login: "○", instalar: "✖", chave: "✖" } as const;

/** O trecho entre crases da dica é o comando a rodar: aparece como código. */
function Dica({ texto }: { texto: string }) {
  return (
    <>
      {texto.split("`").map((parte, i) => (i % 2 === 1 ? <code key={i}>{parte}</code> : <span key={i}>{parte}</span>))}
    </>
  );
}

/**
 * Quem responde no chat. Cada provedor mostra a ferramenta que roda a
 * conversa e, quando ainda não dá para usar, o que falta.
 */
export function Provedores({ aberta, fechar }: { aberta: boolean; fechar: () => void }) {
  const [itens, setItens] = useState<ItemProvedor[]>([]);
  const [busca, setBusca] = useState("");
  const [aviso, setAviso] = useState("");

  useEffect(() => {
    if (!aberta) return;
    setBusca("");
    setAviso("");
    let vivo = true;
    void nucleo.provedores().then((r) => vivo && setItens(r));
    return () => {
      vivo = false;
    };
  }, [aberta]);

  const termo = busca.trim().toLowerCase();
  const visiveis = itens.filter((p) => !termo || `${p.nome} ${p.ferramenta} ${p.modelo}`.toLowerCase().includes(termo));
  const prontos = visiveis.filter((p) => p.estado === "pronto");
  const faltando = visiveis.filter((p) => p.estado !== "pronto");

  const escolher = (p: ItemProvedor) => {
    // Falta instalar ou falta a chave: trocar não adiantaria. A lista fica
    // aberta com o passo à mostra.
    if (p.estado === "instalar" || p.estado === "chave") {
      setAviso(`${p.nome}: ${p.dica}. Depois é só abrir esta lista de novo.`);
      return;
    }
    fechar();
    void nucleo.escolherProvedor(p.nome);
  };

  const item = (p: ItemProvedor) => (
    <Command.Item key={p.nome} value={p.nome} onSelect={() => escolher(p)} data-estado={p.estado}>
      <span className="p-glifo" aria-hidden="true">
        {GLIFO[p.estado]}
      </span>
      <span className="pv-texto">
        <span className="pv-nome">
          {p.nome}
          {p.atual && <span className="chip pv-atual">em uso</span>}
        </span>
        <span className="pv-detalhe">{p.estado === "pronto" ? [p.ferramenta, p.modelo].filter(Boolean).join(" · ") : <Dica texto={p.dica} />}</span>
      </span>
    </Command.Item>
  );

  return (
    <Command.Dialog
      open={aberta}
      onOpenChange={(v) => !v && fechar()}
      label="Quem responde no chat"
      shouldFilter={false}
      className="paleta provedores"
      overlayClassName="paleta-fundo"
      contentClassName="paleta-conteudo"
    >
      <Command.Input value={busca} onValueChange={setBusca} placeholder="Quem responde no chat? ChatGPT, Claude, Kimi, Google…" />
      <Command.List>
        <Command.Empty>Nenhum provedor com esse nome.</Command.Empty>
        {prontos.length > 0 && <Command.Group heading="Prontos para usar">{prontos.map(item)}</Command.Group>}
        {faltando.length > 0 && <Command.Group heading="Falta um passo">{faltando.map(item)}</Command.Group>}
      </Command.List>
      <div className="pv-rodape">
        {aviso || "Cada ferramenta usa a sua própria conta. Falta entrar? Ao escolher, o login abre num card."}
      </div>
    </Command.Dialog>
  );
}
