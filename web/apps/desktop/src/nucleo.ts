// Ponte com o núcleo em Rust (comandos Tauri e eventos do laço).

import { Channel as TauriChannel, invoke as tauriInvoke } from "@tauri-apps/api/core";
import { listen as tauriListen, type UnlistenFn } from "@tauri-apps/api/event";

// ------------------------------------------------------------ transporte
// O MESMO frontend roda na janela Tauri (IPC nativo) e no navegador pelo
// túnel (HTTP/SSE). Esta ponte decide o caminho; o resto do código nem sabe.

const emTauri = typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;

async function invoke<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
  if (emTauri) return tauriInvoke<T>(cmd, args);
  const r = await fetch(`/rpc/${cmd}`, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify(args ?? {}),
  });
  if (r.status === 401) {
    location.href = "/";
    throw new Error("sessão expirada — reabra o link de acesso");
  }
  const j = await r.json().catch(() => ({}) as { dado?: unknown; erro?: string });
  if (!r.ok || j.erro) throw new Error(j.erro ?? `falha em ${cmd}`);
  return (j.dado ?? null) as T;
}

// SSE do estado (o `listen("estado")` no navegador).
let sseEstado: EventSource | null = null;
const ouvintesEstado = new Set<(f: Foto) => void>();
function garantirSSEEstado() {
  if (emTauri || sseEstado) return;
  sseEstado = new EventSource("/eventos");
  sseEstado.addEventListener("estado", (ev) => {
    try {
      const f = JSON.parse((ev as MessageEvent).data) as Foto;
      ouvintesEstado.forEach((cb) => cb(f));
    } catch {
      /* pedaço inválido, ignora */
    }
  });
}

const terminaisSSE = new Map<number, EventSource>();

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

export interface TesteModelo {
  /** `"<provedor>/<modelo>"`. */
  chave: string;
  ok: boolean;
  /** A resposta (sucesso) ou o motivo (falha), curto. */
  texto: string;
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
  /** Nível de permissão do orquestrador: "bypass" | "padrao" | "autonomo". */
  permModo: string;
  status: string;
  avisos: string[];
  decisoes: Decisao[];
  consumo: string;
  /** Opções do seletor de modelo do provedor ativo — a 1ª é sempre "padrão
   * do provedor"; o resto vem da API dele depois de `sincronizarModelos`. */
  modelosOpcoes: string[];
  modelosCarregando: boolean;
  testeModelo: TesteModelo | null;
}

export interface SshHost {
  nome: string;
  host: string;
  usuario: string;
  porta: number;
  /** Caminho da chave privada no seu disco (nunca é copiada). */
  chave: string;
  /** Vale em todos os projetos (uma VPS costuma servir a vários). */
  global: boolean;
}

export interface AcessoRemoto {
  ms: number;
  ip: string;
  ok: boolean;
}

export interface RemotoStatus {
  senhaDefinida: boolean;
  /** 2FA (autenticador) ativo? */
  totpAtivo: boolean;
  /** URL base do túnel enquanto ligado; `null` quando desligado. */
  url: string | null;
  /** Caminho secreto da tela de login (`/entrar/<token>`); com a URL forma o link. */
  caminho: string;
  /** Acessos recentes (ok/falha, hora, IP), para você ver quem tentou entrar. */
  acessos: AcessoRemoto[];
}

export interface EntradaArquivo {
  nome: string;
  /** Relativo à raiz do projeto — é o que volta para `ideLer`/`ideSalvar`. */
  caminho: string;
  pasta: boolean;
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
  sincronizarModelos: () => invoke<void>("sincronizar_modelos"),
  testarModelo: (modelo: string) => invoke<void>("testar_modelo", { modelo }),
  /** A tela virtual ao vivo de uma sandbox aberta pelo orquestrador (`ui_open`),
   * como `data:` URL — `null` sem sandbox aberta com esse nome, ou sem foto ainda. */
  telaViva: (nome: string) => invoke<string | null>("tela_viva", { nome }),
  /** Uma pasta do projeto ativo (raso — expande sob demanda). `""` é a raiz. */
  ideListar: (pasta: string) => invoke<EntradaArquivo[]>("ide_listar", { pasta }),
  ideLer: (caminho: string) => invoke<string>("ide_ler", { caminho }),
  ideSalvar: (caminho: string, conteudo: string) => invoke<void>("ide_salvar", { caminho, conteudo }),
  alterarPastaProjeto: (nome: string, pasta: string) => invoke<string>("alterar_pasta_projeto", { nome, pasta }),
  /** `pasta` = "-" volta a workspace para a pasta do projeto. */
  alterarPastaWorkspace: (indice: number, pasta: string) => invoke<string>("alterar_pasta_workspace", { indice, pasta }),
  abrirTerminal: () => invoke<string>("abrir_terminal"),
  abrirSsh: (nome: string) => invoke<string>("abrir_ssh", { nome }),
  sshListar: () => invoke<SshHost[]>("ssh_listar"),
  sshSalvar: (hosts: SshHost[]) => invoke<string>("ssh_salvar", { hosts }),
  /** Sem `url`, só sobe o container; com `docker`, deixa rodar containers dentro. */
  iniciarSandbox: (nome: string, url: string, docker: boolean) => invoke<string>("iniciar_sandbox", { nome, url, docker }),
  pararSandbox: (nome: string) => invoke<string>("parar_sandbox", { nome }),
  /** Troca o nível de permissão do orquestrador (bypass/padrao/autonomo). */
  definirPermModo: (modo: string) => invoke<string>("definir_perm_modo", { modo }),
  remotoDefinirSenha: (senha: string) => invoke<void>("remoto_definir_senha", { senha }),
  remotoStatus: () => invoke<RemotoStatus>("remoto_status"),
  /** Sobe o servidor local (loopback) se preciso e abre o túnel; devolve a URL. */
  remotoLigar: () => invoke<string>("remoto_ligar"),
  remotoDesligar: () => invoke<void>("remoto_desligar"),
  /** Gera um link novo (invalida o antigo) e derruba as sessões; devolve o caminho. */
  remotoRegenerarToken: () => invoke<string>("remoto_regenerar_token"),
  remotoRevogarSessoes: () => invoke<void>("remoto_revogar_sessoes"),
  /** Começa o 2FA: devolve o otpauth:// e o segredo base32 para o autenticador. */
  remotoTotpIniciar: () => invoke<{ otpauth: string; secret: string }>("remoto_totp_iniciar"),
  remotoTotpAtivar: (codigo: string) => invoke<void>("remoto_totp_ativar", { codigo }),
  remotoTotpDesativar: () => invoke<void>("remoto_totp_desativar"),
  trocarWorkspace: (indice: number) => invoke<void>("trocar_workspace", { indice }),
  focarCard: (indice: number) => invoke<void>("focar_card", { indice }),
  fecharCard: (indice: number) => invoke<void>("fechar_card", { indice }),
  resolverDecisao: (id: string, aprovar: boolean) => invoke<void>("resolver_decisao", { id, aprovar }),
  responderPergunta: (id: string, resposta: string) => invoke<void>("responder_pergunta", { id, resposta }),
  escreverTerminal: (indice: number, dados: string) => invoke<void>("escrever_terminal", { indice, dados }),
  redimensionarTerminal: (indice: number, linhas: number, colunas: number) =>
    invoke<void>("redimensionar_terminal", { indice, linhas, colunas }),
  iterarAgente: (indice: number, texto: string) => invoke<void>("iterar_agente", { indice, texto }),
  /** Base da API da memória. No app é o loopback direto; no navegador (acesso
   * remoto) é `/memoria` na mesma origem — o servidor repassa as leituras. */
  memoriaApi: () => (emTauri ? invoke<string>("memoria_api") : Promise.resolve(`${location.origin}/memoria`)),

  /** Liga um terminal: `aoReceber` ganha a tela atual e depois cada pedaço. */
  assinarTerminal(indice: number, aoReceber: (bytes: Uint8Array) => void): Promise<void> {
    if (emTauri) {
      const canal = new TauriChannel<ArrayBuffer | number[]>();
      canal.onmessage = (dados) => {
        aoReceber(dados instanceof ArrayBuffer ? new Uint8Array(dados) : Uint8Array.from(dados));
      };
      return tauriInvoke<void>("assinar_terminal", { indice, canal });
    }
    // Navegador: SSE com a saída em base64.
    terminaisSSE.get(indice)?.close();
    const es = new EventSource(`/terminal/${indice}`);
    es.onmessage = (ev) => {
      const bin = atob(ev.data);
      const bytes = new Uint8Array(bin.length);
      for (let i = 0; i < bin.length; i++) bytes[i] = bin.charCodeAt(i);
      aoReceber(bytes);
    };
    terminaisSSE.set(indice, es);
    return Promise.resolve();
  },

  aoMudarEstado: (f: (foto: Foto) => void): Promise<UnlistenFn> => {
    if (emTauri) return tauriListen<Foto>("estado", (e) => f(e.payload));
    garantirSSEEstado();
    ouvintesEstado.add(f);
    return Promise.resolve(() => ouvintesEstado.delete(f));
  },
  aoPedir: (f: (pedido: Pedido) => void): Promise<UnlistenFn> => {
    // Os "pedidos" (foco de janela) só fazem sentido no app nativo.
    if (emTauri) return tauriListen<Pedido>("pedido", (e) => f(e.payload));
    return Promise.resolve(() => {});
  },
};
