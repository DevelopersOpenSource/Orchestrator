import "@fontsource/ibm-plex-sans/400.css";
import "@fontsource/ibm-plex-sans/500.css";
import "@fontsource/ibm-plex-sans/600.css";
import "@fontsource/jetbrains-mono/400.css";
import "./styles.css";

import { createGlobe, KIND_LABELS, type GraphData, type GraphNode, type Kind } from "@orchestrator/globe";
import { api, ApiError, type Health, type Memory } from "./api";

const $ = <T extends HTMLElement>(id: string): T => document.getElementById(id) as T;
const TOKEN_KEY = "orchestrator.memory.token";

const el = {
  endereco: $("endereco"),
  estado: $("estado"),
  estadoTexto: $("estado-texto"),
  botaoToken: $<HTMLButtonElement>("botao-token"),
  q: $<HTMLInputElement>("q"),
  busca: $<HTMLFormElement>("busca"),
  filtroProjeto: $<HTMLSelectElement>("filtro-projeto"),
  filtroTipo: $<HTMLSelectElement>("filtro-tipo"),
  filtroOrigem: $<HTMLSelectElement>("filtro-origem"),
  contagem: $("contagem"),
  globo: $("globo"),
  legenda: $("legenda"),
  resultados: $("resultados"),
  alca: $<HTMLButtonElement>("alca"),
  tituloResultados: $("titulo-resultados"),
  tempo: $("tempo"),
  lista: $<HTMLOListElement>("lista-resultados"),
  vazio: $("vazio-resultados"),
  detalhe: $("detalhe"),
  voltar: $<HTMLButtonElement>("voltar"),
  detalheTitulo: $("detalhe-titulo"),
  detalheMeta: $("detalhe-meta"),
  detalheCorpo: $("detalhe-corpo"),
  detalheAcoes: $("detalhe-acoes"),
  editar: $<HTMLButtonElement>("editar"),
  apagar: $<HTMLButtonElement>("apagar"),
  todas: $<HTMLUListElement>("todas"),
  filtroLista: $<HTMLInputElement>("filtro-lista"),
  vazioLista: $("vazio-lista"),
  novaMemoria: $<HTMLButtonElement>("nova-memoria"),
  apiBase: $("api-base"),
  exemplos: $("exemplos"),
  dialogoToken: $<HTMLDialogElement>("dialogo-token"),
  token: $<HTMLInputElement>("token"),
  erroToken: $("erro-token"),
  dialogoMemoria: $<HTMLDialogElement>("dialogo-memoria"),
  formMemoria: $<HTMLFormElement>("form-memoria"),
  tituloForm: $("titulo-form"),
  mTitulo: $<HTMLInputElement>("m-titulo"),
  mCorpo: $<HTMLTextAreaElement>("m-corpo"),
  mOnde: $<HTMLSelectElement>("m-onde"),
  mPrioridade: $<HTMLInputElement>("m-prioridade"),
  erroMemoria: $("erro-memoria"),
  aviso: $("aviso"),
  tpl: $<HTMLTemplateElement>("tpl-resultado"),
};

function lerToken(): string {
  try {
    return sessionStorage.getItem(TOKEN_KEY) ?? "";
  } catch {
    return "";
  }
}

const state = {
  todas: [] as Memory[],
  resultados: null as Memory[] | null,
  resultadoReranqueado: false,
  selecionada: null as Memory | null,
  token: lerToken(),
  editando: null as Memory | null,
  controller: null as AbortController | null,
  health: null as Health | null,
};

const globe = createGlobe(el.globo, {
  onSelect: (node) => selecionarNo(node),
});

// ---------------------------------------------------------------- utilidades

function avisar(texto: string): void {
  el.aviso.textContent = texto;
  el.aviso.hidden = false;
  window.clearTimeout(Number(el.aviso.dataset.timer));
  el.aviso.dataset.timer = String(window.setTimeout(() => (el.aviso.hidden = true), 3500));
}

function quem(m: Memory): string {
  if (m.scope === "global") return "global · dono";
  return m.origin === "agent" ? `IA: ${m.author || "sem nome"}` : "dono";
}

function onde(m: Memory): string {
  return m.scope === "global" ? "todo projeto" : m.project;
}

function projetos(): string[] {
  return [...new Set(state.todas.filter((m) => m.scope !== "global").map((m) => m.project))].sort();
}

function passaFiltros(m: Memory): boolean {
  const projeto = el.filtroProjeto.value;
  if (projeto && m.project !== projeto && m.scope !== "global") return false;
  const tipo = el.filtroTipo.value;
  if (tipo && m.kind !== tipo) return false;
  switch (el.filtroOrigem.value) {
    case "dono":
      return m.origin === "user" && m.scope !== "global";
    case "ia":
      return m.origin === "agent";
    case "global":
      return m.scope === "global";
    default:
      return true;
  }
}

function dataCurta(iso: string): string {
  const d = new Date(iso);
  return Number.isNaN(d.getTime()) ? "" : d.toLocaleDateString("pt-BR", { day: "2-digit", month: "short", year: "numeric" });
}

// ---------------------------------------------------------------- estado do serviço

async function atualizarEstado(): Promise<void> {
  let nivel = "erro";
  let texto = "Serviço da memória fora do ar";
  try {
    const h = await api.health();
    state.health = h;
    if (h.semantic) {
      nivel = "ok";
      texto = "Busca por sentido pronta";
    } else if (h.models.startsWith("baixando")) {
      nivel = "parcial";
      texto = `Baixando os modelos (${h.models.replace("baixando ", "")}) — por enquanto, busca por palavras`;
    } else if (!h.models_ready) {
      nivel = "parcial";
      texto = "Carregando os modelos — por enquanto, busca por palavras";
    } else {
      nivel = "parcial";
      texto = "Índice indisponível — busca por palavras";
    }
  } catch {
    state.health = null;
  }
  el.estado.dataset.nivel = nivel;
  el.estadoTexto.textContent = texto;
  el.estado.title = texto;
  window.setTimeout(atualizarEstado, nivel === "ok" ? 20000 : 3000);
}

// ---------------------------------------------------------------- dados

async function carregar(): Promise<void> {
  try {
    const projeto = el.filtroProjeto.value || undefined;
    const [todas, grafo] = await Promise.all([api.memories(), api.graph(projeto)]);
    state.todas = todas;
    preencherProjetos();
    globe.setData(grafo as GraphData);
    renderResultados();
    renderLista();
    renderExemplos();
  } catch (e) {
    mostrarVazio(el.vazio, "Não consegui falar com o serviço da memória.", mensagem(e));
  }
}

function mensagem(e: unknown): string {
  return e instanceof Error ? e.message : String(e);
}

const telaEstreita = window.matchMedia("(max-width: 760px)");

/** No celular os filtros dividem a largura: rótulo curto em vez de cortado. */
function rotulosDosFiltros(): void {
  const curto = telaEstreita.matches;
  el.filtroProjeto.options[0].text = curto ? "Projeto" : "Todos os projetos";
  el.filtroTipo.options[0].text = curto ? "Tipo" : "Todos os tipos";
  el.filtroOrigem.options[0].text = curto ? "Autor" : "Qualquer autor";
}

function preencherProjetos(): void {
  const atual = el.filtroProjeto.value;
  const nomes = projetos();
  el.filtroProjeto.replaceChildren(new Option("Todos os projetos", ""), ...nomes.map((p) => new Option(p, p)));
  el.filtroProjeto.value = nomes.includes(atual) ? atual : "";
  rotulosDosFiltros();
}

// ---------------------------------------------------------------- busca

let espera: number | undefined;

function buscar(rerank: boolean): void {
  const q = el.q.value.trim();
  window.clearTimeout(espera);
  state.controller?.abort();
  if (!q) {
    state.resultados = null;
    globe.highlight([]);
    renderResultados();
    return;
  }
  const controller = new AbortController();
  state.controller = controller;
  const executar = async () => {
    try {
      const r = await api.search(q, {
        project: el.filtroProjeto.value || undefined,
        kind: el.filtroTipo.value || undefined,
        limit: 20,
        rerank,
        signal: controller.signal,
      });
      state.resultados = r.hits.filter(passaFiltros);
      state.resultadoReranqueado = r.reranked;
      el.tempo.textContent = `${r.took_ms} ms · ${r.reranked ? "reranker" : r.semantic ? "prévia por sentido" : "por palavras"}`;
      renderResultados();
      globe.highlight(state.resultados.map((m) => m.id));
      if (rerank && state.resultados[0]) globe.focus(state.resultados[0].id);
    } catch (e) {
      if ((e as Error).name === "AbortError") return;
      mostrarVazio(el.vazio, "A busca falhou.", mensagem(e));
    }
  };
  if (rerank) void executar();
  else espera = window.setTimeout(() => void executar(), 180);
}

// ---------------------------------------------------------------- resultados

function mostrarVazio(alvo: HTMLElement, titulo: string, texto: string): void {
  const strong = document.createElement("strong");
  strong.textContent = titulo;
  const p = document.createElement("span");
  p.textContent = texto;
  alvo.replaceChildren(strong, p);
  alvo.hidden = false;
}

function itemResultado(m: Memory, maior: number, onClick: () => void): HTMLLIElement {
  const li = el.tpl.content.firstElementChild!.cloneNode(true) as HTMLLIElement;
  const botao = li.querySelector<HTMLButtonElement>(".resultado")!;
  li.querySelector(".r-titulo")!.textContent = m.title;
  const barra = li.querySelector<HTMLElement>(".r-barra")!;
  const score = li.querySelector<HTMLElement>(".r-score")!;
  if (m.score === undefined) {
    barra.remove();
    score.textContent = `prioridade ${m.priority}`;
  } else {
    (barra.firstElementChild as HTMLElement).style.width = `${Math.round((m.score / maior) * 100)}%`;
    score.textContent = m.score.toFixed(2);
    score.title = state.resultadoReranqueado ? "relevância segundo o reranker" : "semelhança de sentido (prévia)";
  }
  li.querySelector(".r-tipo .ponto")!.classList.add(`k-${m.kind}`);
  li.querySelector(".r-tipo-nome")!.textContent = KIND_LABELS[m.kind as Kind] ?? m.kind;
  li.querySelector(".r-onde")!.textContent = `${onde(m)} · ${quem(m)}`;
  botao.addEventListener("click", onClick);
  if (state.selecionada?.id === m.id) botao.setAttribute("aria-current", "true");
  return li;
}

function renderResultados(): void {
  const buscando = state.resultados !== null;
  const lista = buscando
    ? state.resultados!
    : state.todas.filter(passaFiltros).sort((a, b) => b.priority - a.priority || b.updated_at.localeCompare(a.updated_at));
  const maior = Math.max(...lista.map((m) => m.score ?? 0), 0.0001);
  el.lista.replaceChildren(...lista.slice(0, 50).map((m) => itemResultado(m, maior, () => abrirDetalhe(m))));
  el.tituloResultados.textContent = buscando
    ? `${lista.length} resultado${lista.length === 1 ? "" : "s"}`
    : "Memórias";
  if (!buscando) el.tempo.textContent = `${lista.length} de ${state.todas.length}`;
  el.contagem.textContent = `${state.todas.length} memória${state.todas.length === 1 ? "" : "s"}`;

  el.vazio.hidden = true;
  if (state.todas.length === 0) {
    mostrarVazio(
      el.vazio,
      "Ainda não há memórias.",
      "Elas aparecem aqui quando você ou as IAs registram algo. No terminal, Ctrl+Shift+W abre o painel de memória; aqui, use “Editar memórias”.",
    );
  } else if (buscando && lista.length === 0) {
    mostrarVazio(
      el.vazio,
      "Nada relevante para esta pergunta.",
      "A memória só mostra o que tem relação de verdade. Tente outras palavras, tire um filtro, ou veja tudo na aba Lista.",
    );
  } else if (!buscando && lista.length === 0) {
    mostrarVazio(el.vazio, "Nenhuma memória com estes filtros.", "Troque o projeto, o tipo ou o autor lá em cima.");
  }
  if (!state.selecionada) mostrarLista();
}

function mostrarLista(): void {
  el.detalhe.hidden = true;
  el.lista.hidden = false;
  document.querySelector<HTMLElement>(".resultados-cabecalho")!.hidden = false;
}

function abrirDetalhe(m: Memory): void {
  state.selecionada = m;
  el.lista.hidden = true;
  el.vazio.hidden = true;
  document.querySelector<HTMLElement>(".resultados-cabecalho")!.hidden = true;
  el.detalhe.hidden = false;
  el.resultados.dataset.aberta = "true";

  el.detalheTitulo.textContent = m.title;
  const ponto = document.createElement("span");
  ponto.className = `ponto k-${m.kind}`;
  const tipo = document.createElement("span");
  tipo.textContent = KIND_LABELS[m.kind as Kind] ?? m.kind;
  const partes = [`vale em ${onde(m)}`, `escrita por ${quem(m)}`, `importância ${m.priority}`];
  if (m.origin === "user" && m.priority >= 9 && m.kind !== "security") partes.push("regra fixa, vai em todo prompt");
  const data = dataCurta(m.updated_at);
  if (data) partes.push(`atualizada em ${data}`);
  const resto = partes.map((t) => {
    const s = document.createElement("span");
    s.textContent = t;
    return s;
  });
  el.detalheMeta.replaceChildren(ponto, tipo, ...resto);
  el.detalheCorpo.textContent = m.body || "(sem texto)";

  el.detalheAcoes.hidden = !state.token;
  el.editar.hidden = m.origin === "agent";
  el.editar.title = "";
  el.apagar.textContent = "Apagar";
  delete el.apagar.dataset.confirmar;
  globe.focus(m.id);
}

function fecharDetalhe(): void {
  state.selecionada = null;
  renderResultados();
}

function selecionarNo(node: GraphNode | null): void {
  if (!node) return;
  if (node.type === "hub") {
    const projeto = node.id.replace(/^polo:/, "");
    el.filtroProjeto.value = projeto === "global" ? "" : projeto;
    void carregar();
    return;
  }
  const m = state.todas.find((x) => x.id === node.id);
  if (m) abrirDetalhe(m);
}

// ---------------------------------------------------------------- lista completa

function renderLista(): void {
  const termo = el.filtroLista.value.trim().toLowerCase();
  const lista = state.todas.filter((m) => !termo || `${m.title}\n${m.body}`.toLowerCase().includes(termo));
  el.todas.replaceChildren(
    ...lista.map((m) =>
      itemResultado({ ...m, score: undefined }, 1, () => {
        trocarVista("globo");
        abrirDetalhe(m);
      }),
    ),
  );
  el.vazioLista.hidden = true;
  if (lista.length === 0) {
    mostrarVazio(el.vazioLista, state.todas.length ? "Nada com esse texto." : "Ainda não há memórias.", "Ajuste o filtro acima.");
  }
  el.novaMemoria.hidden = !state.token;
}

// ---------------------------------------------------------------- API

function renderExemplos(): void {
  const base = `${location.protocol}//${location.host}`;
  el.apiBase.textContent = base;
  const projeto = projetos()[0] ?? "meu-projeto";
  const exemplos: Array<[string, string, string]> = [
    ["Buscar pelo sentido", "Passa pelo reranker e só devolve o que tem relação.", `curl -G '${base}/api/search' \\\n  --data-urlencode 'q=qual banco o backend usa?' \\\n  --data-urlencode 'project=${projeto}'`],
    ["Prévia instantânea", "Só pelo vetor, para mostrar enquanto se digita.", `curl -G '${base}/api/search' \\\n  --data-urlencode 'q=banco' --data-urlencode 'rerank=false'`],
    ["O que uma IA recebe junto do prompt", "O mesmo texto invisível que o hook injeta.", `curl -G '${base}/api/context' \\\n  --data-urlencode 'project=${projeto}' \\\n  --data-urlencode 'prompt=vou criar a tabela de pedidos'`],
    ["Listar e ler", "", `curl '${base}/api/memories?project=${projeto}'\ncurl '${base}/api/graph'`],
    ["Criar uma memória do dono", "Precisa do token.", `curl -X POST '${base}/api/memories' \\\n  -H "Authorization: Bearer $(cat ~/.config/orchestrator/memory-api.token)" \\\n  -H 'Content-Type: application/json' \\\n  -d '{"project":"${projeto}","kind":"decision","title":"Pagamentos só pelo backend","body":"O front nunca chama o gateway direto.","priority":5}'`],
  ];
  el.exemplos.replaceChildren(
    ...exemplos.map(([titulo, texto, codigo]) => {
      const bloco = document.createElement("div");
      bloco.className = "exemplo";
      const h = document.createElement("h3");
      h.textContent = titulo;
      const p = document.createElement("p");
      p.textContent = texto;
      const pre = document.createElement("pre");
      pre.textContent = codigo;
      bloco.append(h, ...(texto ? [p] : []), pre);
      return bloco;
    }),
  );
}

// ---------------------------------------------------------------- abas

function trocarVista(vista: string): void {
  for (const b of document.querySelectorAll<HTMLButtonElement>(".abas button")) {
    b.setAttribute("aria-selected", String(b.dataset.vista === vista));
  }
  $("vista-globo").hidden = vista !== "globo";
  $("vista-lista").hidden = vista !== "lista";
  $("vista-api").hidden = vista !== "api";
  if (vista === "globo") requestAnimationFrame(ajustarGlobo);
}

function ajustarGlobo(): void {
  const r = el.globo.getBoundingClientRect();
  if (r.width > 0 && r.height > 0) globe.resize(r.width, r.height);
}

// ---------------------------------------------------------------- edição

function atualizarBotaoToken(): void {
  el.botaoToken.textContent = state.token ? "Edição liberada" : "Editar memórias";
  el.botaoToken.title = state.token ? "Clique para esquecer o token nesta aba" : "Criar, editar e apagar memórias por esta página";
  el.novaMemoria.hidden = !state.token;
  if (state.selecionada) el.detalheAcoes.hidden = !state.token;
}

async function validarToken(token: string): Promise<boolean> {
  // Um PUT num id que não existe: token certo → 404, token errado → 401.
  try {
    await api.update(token, "verificacao-de-token", {});
    return true;
  } catch (e) {
    return e instanceof ApiError && e.status === 404;
  }
}

function abrirFormulario(m: Memory | null): void {
  state.editando = m;
  el.tituloForm.textContent = m ? "Editar memória" : "Nova memória";
  el.erroMemoria.hidden = true;
  const opcoes = [new Option("Global — vale em todo projeto", "*global*"), ...projetos().map((p) => new Option(`Projeto ${p}`, p))];
  el.mOnde.replaceChildren(...opcoes);
  el.mOnde.value = m ? (m.scope === "global" ? "*global*" : m.project) : el.filtroProjeto.value || (projetos()[0] ?? "*global*");
  el.mOnde.disabled = Boolean(m);
  el.mTitulo.value = m?.title ?? "";
  el.mCorpo.value = m?.body ?? "";
  el.mPrioridade.value = String(m?.priority ?? 3);
  for (const r of el.formMemoria.querySelectorAll<HTMLInputElement>('input[name="kind"]')) {
    r.checked = r.value === (m?.kind ?? "architecture");
  }
  el.dialogoMemoria.showModal();
  el.mTitulo.focus();
}

async function salvarMemoria(): Promise<void> {
  const kind = (el.formMemoria.querySelector<HTMLInputElement>('input[name="kind"]:checked')?.value ?? "architecture") as Kind;
  const dados = {
    kind,
    title: el.mTitulo.value.trim(),
    body: el.mCorpo.value.trim(),
    priority: Number(el.mPrioridade.value),
  };
  try {
    let salva: Memory;
    if (state.editando) {
      salva = await api.update(state.token, state.editando.id, dados);
    } else {
      const destino = el.mOnde.value;
      salva = await api.create(state.token, destino === "*global*" ? { ...dados, scope: "global" } : { ...dados, project: destino });
    }
    el.dialogoMemoria.close();
    avisar(state.editando ? "Memória atualizada." : "Memória criada — entra no índice em instantes.");
    await carregar();
    abrirDetalhe(state.todas.find((x) => x.id === salva.id) ?? salva);
  } catch (e) {
    el.erroMemoria.textContent = mensagem(e);
    el.erroMemoria.hidden = false;
  }
}

async function apagarSelecionada(): Promise<void> {
  const m = state.selecionada;
  if (!m) return;
  if (!el.apagar.dataset.confirmar) {
    el.apagar.dataset.confirmar = "1";
    el.apagar.textContent = "Clique de novo para apagar";
    window.setTimeout(() => {
      delete el.apagar.dataset.confirmar;
      el.apagar.textContent = "Apagar";
    }, 4000);
    return;
  }
  try {
    await api.remove(state.token, m.id);
    avisar("Memória apagada.");
    state.selecionada = null;
    await carregar();
  } catch (e) {
    avisar(mensagem(e));
  }
}

// ---------------------------------------------------------------- ligações

function ligar(): void {
  el.endereco.textContent = location.host;
  el.legenda.replaceChildren(
    ...(Object.keys(KIND_LABELS) as Kind[]).map((k) => {
      const li = document.createElement("li");
      const p = document.createElement("span");
      p.className = `ponto k-${k}`;
      li.append(p, KIND_LABELS[k]);
      return li;
    }),
    (() => {
      const li = document.createElement("li");
      const p = document.createElement("span");
      p.className = "ponto ponto-polo";
      li.append(p, "polo (global ou projeto)");
      return li;
    })(),
  );

  for (const b of document.querySelectorAll<HTMLButtonElement>(".abas button")) {
    b.addEventListener("click", () => trocarVista(b.dataset.vista ?? "globo"));
  }
  el.q.addEventListener("input", () => buscar(false));
  el.busca.addEventListener("submit", (e) => {
    e.preventDefault();
    buscar(true);
  });
  el.q.addEventListener("keydown", (e) => {
    if (e.key === "Escape") {
      el.q.value = "";
      buscar(false);
    }
  });
  for (const f of [el.filtroTipo, el.filtroOrigem]) {
    f.addEventListener("change", () => (el.q.value.trim() ? buscar(true) : renderResultados()));
  }
  el.filtroProjeto.addEventListener("change", () => {
    void carregar().then(() => el.q.value.trim() && buscar(true));
  });
  el.voltar.addEventListener("click", fecharDetalhe);
  el.alca.addEventListener("click", () => {
    el.resultados.dataset.aberta = el.resultados.dataset.aberta === "true" ? "false" : "true";
  });
  el.filtroLista.addEventListener("input", renderLista);

  el.botaoToken.addEventListener("click", () => {
    if (state.token) {
      state.token = "";
      try {
        sessionStorage.removeItem(TOKEN_KEY);
      } catch {
        /* sem armazenamento: só nesta página */
      }
      atualizarBotaoToken();
      avisar("Token esquecido nesta aba.");
      return;
    }
    el.erroToken.hidden = true;
    el.token.value = "";
    el.dialogoToken.showModal();
  });
  $<HTMLFormElement>("form-token").addEventListener("submit", async (e) => {
    const submissor = (e as SubmitEvent).submitter as HTMLButtonElement | null;
    if (submissor?.value !== "salvar") return;
    e.preventDefault();
    const token = el.token.value.trim();
    if (await validarToken(token)) {
      state.token = token;
      try {
        sessionStorage.setItem(TOKEN_KEY, token);
      } catch {
        /* sem armazenamento: vale até recarregar */
      }
      el.dialogoToken.close();
      atualizarBotaoToken();
      avisar("Edição liberada nesta aba.");
    } else {
      el.erroToken.textContent = "Este token não confere. Copie de novo do arquivo memory-api.token.";
      el.erroToken.hidden = false;
    }
  });
  el.formMemoria.addEventListener("submit", (e) => {
    const submissor = (e as SubmitEvent).submitter as HTMLButtonElement | null;
    if (submissor?.value !== "salvar") return;
    e.preventDefault();
    void salvarMemoria();
  });
  el.novaMemoria.addEventListener("click", () => abrirFormulario(null));
  el.editar.addEventListener("click", () => state.selecionada && abrirFormulario(state.selecionada));
  el.apagar.addEventListener("click", () => void apagarSelecionada());

  document.addEventListener("keydown", (e) => {
    const digitando = e.target instanceof HTMLInputElement || e.target instanceof HTMLTextAreaElement || e.target instanceof HTMLSelectElement;
    if (e.key === "/" && !digitando) {
      e.preventDefault();
      trocarVista("globo");
      el.q.focus();
    }
  });

  new ResizeObserver(ajustarGlobo).observe(el.globo);
  telaEstreita.addEventListener("change", rotulosDosFiltros);
  rotulosDosFiltros();
  atualizarBotaoToken();
}

/** Links diretos: `?q=pergunta`, `&projeto=nome`, `&vista=lista|api`. */
function aplicarEndereco(): void {
  const params = new URLSearchParams(location.search);
  const vista = params.get("vista");
  if (vista === "lista" || vista === "api") trocarVista(vista);
  const projeto = params.get("projeto");
  if (projeto && [...el.filtroProjeto.options].some((o) => o.value === projeto)) el.filtroProjeto.value = projeto;
  const q = params.get("q");
  if (q) {
    el.q.value = q;
    buscar(true);
  }
}

ligar();
void atualizarEstado();
void carregar().then(aplicarEndereco);
