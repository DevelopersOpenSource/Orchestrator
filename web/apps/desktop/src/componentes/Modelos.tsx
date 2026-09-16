import { useEffect, useState } from "react";
import { Command } from "cmdk";
import { nucleo, type Foto } from "../nucleo";

const PADRAO = "padrão do provedor";

/**
 * O modelo do provedor ativo. Sincroniza com a API dele ao abrir (quando ela
 * existe) e deixa testar um modelo com um "." antes de escolher.
 */
export function Modelos({ aberta, fechar, foto }: { aberta: boolean; fechar: () => void; foto: Foto }) {
  const [busca, setBusca] = useState("");

  useEffect(() => {
    if (!aberta) return;
    setBusca("");
    void nucleo.sincronizarModelos();
  }, [aberta]);

  const termo = busca.trim().toLowerCase();
  const opcoes = foto.modelosOpcoes.filter((m) => !termo || m.toLowerCase().includes(termo));

  const escolher = (m: string) => {
    fechar();
    void nucleo.enviar(m === PADRAO ? "/modelo" : `/modelo ${m}`);
  };

  const teste = (m: string) => (foto.testeModelo?.chave === `${foto.provedor}/${m}` ? foto.testeModelo : null);

  return (
    <Command.Dialog
      open={aberta}
      onOpenChange={(v) => !v && fechar()}
      label={`Modelo de ${foto.provedor}`}
      shouldFilter={false}
      className="paleta provedores"
      overlayClassName="paleta-fundo"
      contentClassName="paleta-conteudo"
    >
      <Command.Input value={busca} onValueChange={setBusca} placeholder={`Modelo de ${foto.provedor}…`} />
      <Command.List>
        <Command.Empty>Nenhum modelo com esse nome.</Command.Empty>
        <Command.Group heading={foto.modelosCarregando ? `${foto.provedor} — sincronizando com a API…` : foto.provedor}>
          {opcoes.map((m) => {
            const atual = m === (foto.modelo || PADRAO);
            const t = teste(m);
            return (
              <Command.Item key={m} value={m} onSelect={() => escolher(m)}>
                <span className="p-glifo" aria-hidden="true">
                  {atual ? "●" : " "}
                </span>
                <span className="pv-texto">
                  <span className="pv-nome">{m}</span>
                  {t && (
                    <span className={`pv-detalhe modelo-teste ${t.ok ? "modelo-teste-ok" : "modelo-teste-erro"}`}>
                      {t.ok ? "✔" : "✖"} {t.texto}
                    </span>
                  )}
                </span>
                {m !== PADRAO && (
                  <button
                    className="botao-leve modelo-testar"
                    onClick={(e) => {
                      e.stopPropagation();
                      void nucleo.testarModelo(m);
                    }}
                    title={`Manda "." para ${m} e mostra se ele responde`}
                  >
                    Testar
                  </button>
                )}
              </Command.Item>
            );
          })}
        </Command.Group>
      </Command.List>
      <div className="pv-rodape">Testar manda uma mensagem mínima só para conferir que o modelo processa e responde.</div>
    </Command.Dialog>
  );
}
