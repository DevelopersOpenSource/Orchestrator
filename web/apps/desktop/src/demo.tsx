// Demonstração da interface sem o núcleo: os comandos do Tauri são simulados
// com dados de exemplo. Serve para conferir a tela num navegador comum
// (`npm run dev` e abrir /demo.html) — o app de verdade usa main.tsx.

import { mockIPC } from "@tauri-apps/api/mocks";
import type { Foto, ItemPaleta, ItemProvedor } from "./nucleo";

const ESC = "\x1b";
const frontend = [
  `${ESC}[2m>${ESC}[0m crie a página de checkout e rode os testes`,
  "",
  `${ESC}[32m●${ESC}[0m orchestrator - retrieve_memory (loja)`,
  `  ${ESC}[2m⎿${ESC}[0m  3 memórias: Vite + React, testes`,
  "",
  `${ESC}[32m●${ESC}[0m Write(src/pages/Checkout.tsx)`,
  `  ${ESC}[2m⎿${ESC}[0m  Wrote 118 lines`,
  "",
  `${ESC}[32m●${ESC}[0m Bash(npm run test -- checkout)`,
  `  ${ESC}[2m⎿${ESC}[0m  ✓ Checkout.test.tsx (6 tests) 412ms`,
  "",
  `${ESC}[36m✻${ESC}[0m Resumindo… ${ESC}[2m(1m 12s · ↓ 2.1k tokens)${ESC}[0m`,
].join("\r\n");
const backend = [
  `${ESC}[2m›${ESC}[0m aplique as migrações de pedidos e rode os testes`,
  "",
  "• Ran sqlx migrate run",
  "  └ Applied 20260911_pedidos.sql",
  "    Applied 20260912_pagamentos.sql",
  "• Ran cargo test",
  `  └ test result: ${ESC}[32mok${ESC}[0m. 48 passed; 0 failed`,
  "",
  `${ESC}[33m•${ESC}[0m Quer rodar: git push --force origin main`,
  `  └ ${ESC}[33maguardando o dono${ESC}[0m`,
].join("\r\n");

const foto: Foto = {
  projetos: ["loja", "Orchestrator", "site-institucional"],
  projeto: "loja",
  workspace: 0,
  workspaces: [
    {
      numero: 1,
      pasta: "/home/dono/projetos/loja",
      pastaPropria: false,
      focado: 0,
      cards: [
        { indice: 0, nome: "frontend", tipo: "cli", estado: "trabalhando", detalhe: "crie a página de checkout", saida: "", autopilot: "", consumo: "" },
        { indice: 1, nome: "backend", tipo: "cli", estado: "ociosa", detalhe: "", saida: "", autopilot: "", consumo: "" },
        {
          indice: 2,
          nome: "Agente #1",
          tipo: "agente",
          estado: "trabalhando",
          detalhe: "revisar a acessibilidade do checkout e corrigir o que faltar",
          saida: "✔ passo 2: labels ligados aos campos de cartão e CEP\n✔ passo 3: foco visível nos botões, contraste 4.8:1\n⚙ Edit src/pages/Checkout.tsx · aria-live no resumo do pedido",
          autopilot: "autopilot 3/10",
          consumo: "12,8k → 2,4k tok · US$ 0,0421",
        },
      ],
    },
    { numero: 2, pasta: "/home/dono/projetos/loja", pastaPropria: false, focado: 0, cards: [{ indice: 0, nome: "migrações", tipo: "cli", estado: "ociosa", detalhe: "", saida: "", autopilot: "", consumo: "" }] },
    { numero: 3, pasta: "/home/dono/projetos/loja", pastaPropria: false, focado: 0, cards: [] },
    { numero: 4, pasta: "/home/dono/projetos/loja", pastaPropria: false, focado: 0, cards: [] },
  ],
  chat: [
    { quem: "você", texto: "abra uma CLI chamada frontend e mande criar a página de checkout" },
    { quem: "orchestrator", texto: "Abri frontend com o claude na workspace 1 e mandei a tarefa. Pela memória, o front usa Vite + React e nenhuma tarefa fecha sem rodar os testes; passei as duas coisas junto." },
    { quem: "decisão", texto: "⚠ [loja] backend pede confirmação: git push --force origin main\n/aprovar 3f9c1a2b · /negar 3f9c1a2b" },
    { quem: "você", texto: "e o backend, terminou as migrações?" },
  ],
  chatOcupado: true,
  chatParcial: "Terminou. O backend está ocioso há 2 min: aplicou as migrações e o cargo test passou com 48 testes.",
  chatPensando: "",
  fila: null,
  provedor: "claude",
  modelo: "opus",
  postura: "autônomo",
  status: "CLI \"frontend\" aberta na workspace 1 (claude)",
  avisos: [],
  decisoes: [
    {
      id: "3f9c1a2b-5b19-4d8e",
      projeto: "loja",
      resumo: "git push --force origin main",
      criada: "2026-09-12 12:05",
      revisor: "dono",
      pedidoPor: "backend",
      tipo: "acao",
      opcoes: [],
      multipla: false,
      nota: "reescreve o histórico do main, e o pedido era só aplicar as migrações",
    },
    {
      id: "7a21c0de-1f44-4b0a",
      projeto: "loja",
      resumo: "O checkout deve aceitar quais formas de pagamento no lançamento?",
      criada: "2026-09-12 12:07",
      revisor: "dono",
      pedidoPor: "orquestrador",
      tipo: "pergunta",
      opcoes: ["Cartão de crédito", "Pix", "Boleto"],
      multipla: true,
      nota: "",
    },
    {
      id: "c90b3e11-8d2a-4e77",
      projeto: "loja",
      resumo: "[migrações] sqlx migrate run",
      criada: "2026-09-12 12:08",
      revisor: "orquestrador",
      pedidoPor: "backend",
      tipo: "acao",
      opcoes: [],
      multipla: false,
      nota: "",
    },
  ],
  consumo: "18,4k → 3,2k tok · US$ 0,19",
};

const paleta: ItemPaleta[] = [
  { nome: "/cli", uso: "/cli <nome> [comando]", sobre: "abre uma CLI real nomeada nesta workspace", estado: "argumento", motivo: "precisa do nome da CLI" },
  { nome: "/agente", uso: "/agente <tarefa>", sobre: "agente headless que trabalha sozinho num card", estado: "argumento", motivo: "precisa da tarefa" },
  { nome: "/modelo", uso: "/modelo [nome]", sobre: "modelo do chat (sem nome abre o seletor)", estado: "pronto", motivo: "" },
  { nome: "/provedor", uso: "/provedor [nome]", sobre: "quem responde no chat: Claude, ChatGPT, Kimi, Google… (sem nome abre a lista)", estado: "pronto", motivo: "" },
  { nome: "/pasta", uso: "/pasta <caminho|->", sobre: "pasta desta workspace (- volta à do projeto)", estado: "pronto", motivo: "" },
  { nome: "/projeto", uso: "/projeto [nome]", sobre: "troca de projeto (restaura a conversa dele)", estado: "pronto", motivo: "" },
  { nome: "/aprovar", uso: "/aprovar [id]", sobre: "aprova a decisão pendente", estado: "pronto", motivo: "" },
  { nome: "/negar", uso: "/negar [id]", sobre: "nega a decisão pendente", estado: "pronto", motivo: "" },
  { nome: "/auto", uso: "/auto [on|off]", sobre: "avisar o orquestrador quando uma CLI concluir", estado: "argumento", motivo: "agora: avisos ligados" },
  { nome: "/caps", uso: "/caps", sobre: "o que a build do claude suporta", estado: "indisponivel", motivo: "o binário `claude` não está no PATH" },
  { nome: "/ajuda", uso: "/ajuda", sobre: "abre o manual completo", estado: "pronto", motivo: "" },
];

const provedores: ItemProvedor[] = [
  { nome: "Claude Code (CLI)", ferramenta: "Claude Code", modelo: "", estado: "pronto", dica: "", atual: true },
  { nome: "ChatGPT (Codex)", ferramenta: "Codex", modelo: "", estado: "pronto", dica: "", atual: false },
  { nome: "Kimi Code", ferramenta: "Kimi Code", modelo: "", estado: "login", dica: "entre na conta Kimi: `kimi login`", atual: false },
  { nome: "Google (Antigravity)", ferramenta: "Antigravity", modelo: "", estado: "login", dica: "entre na conta Google: rode `agy` uma vez", atual: false },
  { nome: "GLM · Claude Code", ferramenta: "Claude Code", modelo: "", estado: "chave", dica: "falta a chave: defina ZHIPU_API_KEY e reabra o Orchestrator", atual: false },
  { nome: "Groq · OpenCode", ferramenta: "OpenCode", modelo: "groq/openai/gpt-oss-120b", estado: "instalar", dica: "OpenCode não está instalado — instale com: npm i -g opencode-ai", atual: false },
  { nome: "Ollama (local)", ferramenta: "HTTP", modelo: "llama3.2", estado: "pronto", dica: "", atual: false },
];

mockIPC(async (cmd, args) => {
  const a = (args ?? {}) as Record<string, unknown>;
  switch (cmd) {
    case "estado":
      return foto;
    case "provedores":
      return provedores;
    case "escolher_provedor":
      return `chat com ${String(a.nome)}`;
    case "paleta":
      return paleta.filter((p) => p.nome.startsWith(String(a.texto ?? "/").split(" ")[0] || "/"));
    case "memoria_api":
      return "http://127.0.0.1:10000";
    case "assinar_terminal": {
      const canal = a.canal as { id: number };
      const texto = a.indice === 0 ? frontend : backend;
      const bytes = new TextEncoder().encode(texto);
      setTimeout(() => {
        const mensagem = { index: 0, message: bytes.buffer };
        const internos = (window as unknown as { __TAURI_INTERNALS__?: { runCallback?: (id: number, m: unknown) => void } }).__TAURI_INTERNALS__;
        if (typeof internos?.runCallback === "function") internos.runCallback(canal.id, mensagem);
        else (window as unknown as Record<string, (m: unknown) => void>)[`_${canal.id}`]?.(mensagem);
      }, 50);
      return null;
    }
    default:
      return null;
  }
});

const params = new URLSearchParams(location.search);
await import("./main");
if (params.get("paleta")) {
  setTimeout(() => window.dispatchEvent(new KeyboardEvent("keydown", { key: "p", ctrlKey: true, shiftKey: true })), 400);
}
if (params.get("vista") === "decisoes") {
  setTimeout(() => window.dispatchEvent(new KeyboardEvent("keydown", { key: "F2" })), 400);
}
if (params.get("provedores")) {
  setTimeout(() => (document.querySelector(".chat-topo button.chip") as HTMLButtonElement | null)?.click(), 400);
}
if (params.get("manual")) {
  setTimeout(() => window.dispatchEvent(new KeyboardEvent("keydown", { key: "F1" })), 400);
}
