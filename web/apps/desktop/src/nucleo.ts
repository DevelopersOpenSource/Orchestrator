// Ponte com o núcleo em Rust (comandos Tauri e eventos do laço).

import { Channel, invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

export interface Linha {
  quem: string;
  texto: string;
}

export type TipoCard = "cli" | "terminal" | "agente";
export type EstadoCard = "iniciando" | "trabalhando" | "ociosa" | "encerrada";

export interface Card {
  indice: number;
  nome: string;
  tipo: TipoCard;
  estado: EstadoCard;
  detalhe: string;
  saida: string;
  autopilot: string;
  consumo: string;
}

export interface Workspace {
  numero: number;
  pasta: string;
  pastaPropria: boolean;
  focado: number;
  cards: Card[];
}

export interface Decisao {
  id: string;
  projeto: string;
  resumo: string;
  criada: string;
  /** Quem decide: o dono, ou o orquestrador no modo autônomo. */
  revisor: "dono" | "orquestrador";
  pedidoPor: string;
  tipo: "acao" | "pergunta";
  opcoes: string[];
  multipla: boolean;
  /** Por que o orquestrador passou ao dono. */
  nota: string;
}

export interface Foto {
  projetos: string[];
  projeto: string;
  workspace: number;
  workspaces: Workspace[];
  chat: Linha[];
  chatOcupado: boolean;
  chatParcial: string;
  chatPensando: string;
  fila: string | null;
  provedor: string;
  modelo: string;
  postura: string;
  status: string;
  avisos: string[];
  decisoes: Decisao[];
  consumo: string;
}

export interface ItemPaleta {
  nome: string;
  uso: string;
  sobre: string;
  estado: "pronto" | "argumento" | "indisponivel";
  motivo: string;
}

export interface ItemProvedor {
  nome: string;
  ferramenta: string;
  modelo: string;
  /** pronto · login (falta entrar na conta) · instalar · chave */
  estado: "pronto" | "login" | "instalar" | "chave";
  dica: string;
  atual: boolean;
}

/** O que o núcleo pede à janela (foco, manual, escolha de modelo ou de provedor). */
export type Pedido = "focar-grade" | "focar-chat" | "escolher-modelo" | "escolher-provedor" | "abrir-manual";

export const nucleo = {
  estado: () => invoke<Foto>("estado"),
  enviar: (texto: string) => invoke<void>("enviar", { texto }),
  paleta: (texto: string) => invoke<ItemPaleta[]>("paleta", { texto }),
  provedores: () => invoke<ItemProvedor[]>("provedores"),
  /** Devolve o que aconteceu: trocou, abriu o login num card, ou o que falta. */
  escolherProvedor: (nome: string) => invoke<string>("escolher_provedor", { nome }),
  trocarWorkspace: (indice: number) => invoke<void>("trocar_workspace", { indice }),
  focarCard: (indice: number) => invoke<void>("focar_card", { indice }),
  fecharCard: (indice: number) => invoke<void>("fechar_card", { indice }),
  resolverDecisao: (id: string, aprovar: boolean) => invoke<void>("resolver_decisao", { id, aprovar }),
  responderPergunta: (id: string, resposta: string) => invoke<void>("responder_pergunta", { id, resposta }),
  escreverTerminal: (indice: number, dados: string) => invoke<void>("escrever_terminal", { indice, dados }),
  redimensionarTerminal: (indice: number, linhas: number, colunas: number) =>
    invoke<void>("redimensionar_terminal", { indice, linhas, colunas }),
  iterarAgente: (indice: number, texto: string) => invoke<void>("iterar_agente", { indice, texto }),
  memoriaApi: () => invoke<string>("memoria_api"),

  /** Liga um terminal: `aoReceber` ganha a tela atual e depois cada pedaço. */
  assinarTerminal(indice: number, aoReceber: (bytes: Uint8Array) => void): Promise<void> {
    const canal = new Channel<ArrayBuffer | number[]>();
    canal.onmessage = (dados) => {
      aoReceber(dados instanceof ArrayBuffer ? new Uint8Array(dados) : Uint8Array.from(dados));
    };
    return invoke<void>("assinar_terminal", { indice, canal });
  },

  aoMudarEstado: (f: (foto: Foto) => void): Promise<UnlistenFn> => listen<Foto>("estado", (e) => f(e.payload)),
  aoPedir: (f: (pedido: Pedido) => void): Promise<UnlistenFn> => listen<Pedido>("pedido", (e) => f(e.payload)),
};
