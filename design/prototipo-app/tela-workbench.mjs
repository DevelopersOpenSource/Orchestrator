// Workbench: chat do orquestrador + grade de CLIs reais.
import { ico, estado } from './comum.mjs';
import { cabecalho, barraLateral, barraStatus } from './cromo.mjs';

const papel = (quem, cor, hora) =>
  `<div style="display:flex;align-items:baseline;gap:8px;margin-bottom:4px"><span style="font-weight:600;font-size:12.5px;color:${cor}">${quem}</span><span class="mono" style="font-size:11px;color:var(--tx3)">${hora}</span></div>`;

const ferramenta = (ok, nome, arg) =>
  `<div style="display:flex;gap:8px;align-items:baseline;font-size:11.5px;line-height:1.7" class="mono"><span style="color:${ok ? 'var(--ok)' : 'var(--er)'}">${ok ? '✔' : '✖'}</span><span style="color:var(--tx)">${nome}</span><span class="trunc" style="color:var(--tx3)">${arg}</span></div>`;

export function chat({ decisao = true } = {}) {
  return `<section style="width:392px;flex:none;display:flex;flex-direction:column;background:var(--bg);border-right:1px solid var(--bd)">
  <div style="height:40px;display:flex;align-items:center;gap:8px;padding:0 10px 0 14px;border-bottom:1px solid var(--bd)">
    <span style="font-weight:600">Chat</span>
    <span class="mono" style="display:inline-flex;align-items:center;gap:4px;font-size:11.5px;padding:2px 6px 2px 8px;border:1px solid var(--bd2);border-radius:5px;color:var(--tx2)">claude · opus ${ico('down', 'width:12px;height:12px')}</span>
    <div style="flex:1"></div>
    <span style="color:var(--tx3);display:flex">${ico('panel')}</span>
  </div>
  <div style="flex:1;overflow:hidden;padding:16px 16px 8px;display:flex;flex-direction:column;gap:18px">
    <div>${papel('você', 'var(--tx)', '12:02')}<div>abra uma CLI chamada frontend e mande criar a página de checkout</div></div>
    <div>${papel('orchestrator', 'var(--ac)', '12:02')}
      <div style="color:var(--tx);text-wrap:pretty">Abri <b style="font-weight:600">frontend</b> com o claude na workspace 1 e mandei a tarefa. Pela memória, o front usa Vite + React e nenhuma tarefa fecha sem rodar os testes. Passei as duas coisas junto.</div>
      <div style="margin-top:8px;padding:8px 10px;border:1px solid var(--bd);border-radius:6px;background:var(--s1)">
        ${ferramenta(true, 'retrieve_memory', 'loja · “página de checkout” · 3 achados')}
        ${ferramenta(true, 'cli_start', 'frontend · claude')}
        ${ferramenta(true, 'cli_send', 'frontend · 214 caracteres')}
      </div>
    </div>
    ${decisao ? `<div style="border:1px solid var(--av);border-radius:8px;background:var(--av-bg);padding:10px 12px">
      <div style="display:flex;align-items:center;gap:6px;color:var(--av);font-weight:600;font-size:12.5px">${ico('shield')}Decisão pendente · backend</div>
      <div class="mono" style="margin:8px 0 4px;font-size:12px;color:var(--tx)">git push --force origin main</div>
      <div style="font-size:12px;color:var(--tx2)">Pede confirmação pela regra “push forçado só com o dono”.</div>
      <div style="display:flex;gap:6px;margin-top:10px"><span class="btn" style="height:28px">Negar</span><span class="btn btn-p" style="height:28px">Aprovar</span><div style="flex:1"></div><span class="kbd" style="align-self:center">F2</span></div>
    </div>` : ''}
    <div>${papel('você', 'var(--tx)', '12:06')}<div>e o backend, terminou as migrações?</div></div>
    <div>${papel('orchestrator', 'var(--ac)', '12:06')}
      <div style="color:var(--tx);text-wrap:pretty">Terminou. O backend está ocioso há 2 min: aplicou 3 migrações e o <span class="mono" style="font-size:12px">cargo test</span> passou com 48 testes<span style="display:inline-block;width:7px;height:14px;background:var(--ac);vertical-align:-2px;margin-left:2px"></span></div>
      <div style="margin-top:6px;font-size:12px;color:var(--tx3)">digitando · <span class="mono" style="font-size:11px">1,2k → 310 tok</span></div>
    </div>
  </div>
  <div style="padding:10px 12px 12px">
    <div style="border:1px solid var(--bd2);border-radius:8px;background:var(--s1)">
      <div style="padding:10px 12px 6px;color:var(--tx3);min-height:44px">Mensagem para o orquestrador, ou <span class="mono" style="color:var(--tx2)">/</span> para comandos</div>
      <div style="display:flex;align-items:center;gap:8px;padding:6px 6px 6px 12px;font-size:12px;color:var(--tx3)">
        <span>Enter envia · Shift+Enter quebra linha</span><div style="flex:1"></div>
        <span class="btn btn-p" style="width:28px;height:28px;padding:0;justify-content:center">${ico('send')}</span>
      </div>
    </div>
  </div>
</section>`;
}

const linha = (t, cor = 'var(--tx)') => `<div style="color:${cor};white-space:pre">${t}</div>`;

function card({ nome, chip, estadoHtml, atalho, foco = false, corpo, rodape = '' }) {
  return `<div style="flex:1;display:flex;flex-direction:column;min-width:0;min-height:0;border:1px solid ${foco ? 'var(--ac)' : 'var(--bd)'};border-radius:8px;background:var(--term);overflow:hidden">
  <div style="height:34px;flex:none;display:flex;align-items:center;gap:8px;padding:0 6px 0 10px;border-bottom:1px solid var(--bd);background:var(--s1)">
    <span style="font-weight:600">${nome}</span>
    <span class="mono" style="font-size:11px;padding:1px 6px;border:1px solid var(--bd2);border-radius:4px;color:var(--tx2)">${chip}</span>
    ${estadoHtml}
    <div style="flex:1"></div>
    <span class="kbd">${atalho}</span>
    <span style="display:flex;color:var(--tx3);padding:4px">${ico('expand')}</span>
    <span style="display:flex;color:var(--tx3);padding:4px">${ico('x')}</span>
  </div>
  <div class="mono" style="flex:1;overflow:hidden;padding:10px 12px;font-size:11.5px;line-height:17px">${corpo}</div>
  ${rodape}
</div>`;
}

export function grade() {
  const frontend = card({
    nome: 'frontend', chip: 'claude', atalho: 'Alt 1', foco: true,
    estadoHtml: `${estado('ac', 'trabalhando')}<span class="mono" style="font-size:11px;color:var(--tx3)">1m12s</span>`,
    corpo: [
      linha('<span style="color:var(--tx3)">&gt;</span> crie a página de checkout e rode os testes'),
      linha(' '),
      linha('<span style="color:var(--ok)">●</span> orchestrator - retrieve_memory (loja)'),
      linha('  <span style="color:var(--tx3)">⎿</span>  3 memórias: Vite + React, testes', 'var(--tx2)'),
      linha(' '),
      linha('<span style="color:var(--ok)">●</span> Read(src/routes/index.tsx)'),
      linha('  <span style="color:var(--tx3)">⎿</span>  Read 42 lines', 'var(--tx2)'),
      linha(' '),
      linha('<span style="color:var(--ok)">●</span> Write(src/pages/Checkout.tsx)'),
      linha('  <span style="color:var(--tx3)">⎿</span>  Wrote 118 lines', 'var(--tx2)'),
      linha(' '),
      linha('<span style="color:var(--ok)">●</span> Bash(npm run test -- checkout)'),
      linha('  <span style="color:var(--tx3)">⎿</span>  ✓ Checkout.test.tsx (6 tests) 412ms', 'var(--tx2)'),
      linha(' '),
      linha('<span style="color:var(--ac)">✻</span> Resumindo… <span style="color:var(--tx3)">(1m 12s · ↓ 2.1k tokens)</span>'),
    ].join(''),
  });
  const backend = card({
    nome: 'backend', chip: 'codex', atalho: 'Alt 2',
    estadoHtml: estado('ok', 'ociosa'),
    corpo: [
      linha('<span style="color:var(--tx3)">›</span> aplique as migrações de pedidos e rode os testes'),
      linha(' '),
      linha('<span style="color:var(--tx2)">•</span> Ran sqlx migrate run'),
      linha('  <span style="color:var(--tx3)">└</span> Applied 20260911_pedidos.sql', 'var(--tx2)'),
      linha('    Applied 20260911_itens.sql', 'var(--tx2)'),
      linha('    Applied 20260912_pagamentos.sql', 'var(--tx2)'),
      linha('<span style="color:var(--tx2)">•</span> Ran cargo test'),
      linha('  <span style="color:var(--tx3)">└</span> test result: <span style="color:var(--ok)">ok</span>. 48 passed; 0 failed', 'var(--tx2)'),
      linha(' '),
      linha('<span style="color:var(--av)">•</span> Quer rodar: git push --force origin main'),
      linha('  <span style="color:var(--tx3)">└</span> aguardando o dono (F2)', 'var(--av)'),
    ].join(''),
  });
  const agente = card({
    nome: 'Agente #1', chip: 'opus · effort:high', atalho: 'Alt 3',
    estadoHtml: `${estado('ac', 'trabalhando')}<span class="mono" style="font-size:11px;color:var(--tx3)">auto 3/10</span>`,
    corpo: [
      linha('<span style="color:var(--tx3)">tarefa:</span> revisar a acessibilidade do checkout e corrigir o que faltar'),
      linha(' '),
      linha('✔ passo 2: labels ligados aos campos de cartão e CEP', 'var(--tx2)'),
      linha('✔ passo 3: foco visível nos botões, contraste 4.8:1', 'var(--tx2)'),
      linha('<span style="color:var(--ac)">⚙</span> Edit src/pages/Checkout.tsx · aria-live no resumo do pedido'),
      linha('<span style="color:var(--tx3)">trabalhando · 38s</span>'),
    ].join(''),
    rodape: `<div style="flex:none;display:flex;align-items:center;gap:10px;padding:8px 10px;border-top:1px solid var(--bd);background:var(--s1)">
      <span style="color:var(--tx3)">▸</span><span style="flex:1;color:var(--tx3)">Digite para iterar (vai para a fila)</span>
      <span class="mono" style="font-size:11px;color:var(--tx3)">⟳ 12,8k → 2,4k tok · US$ 0,0421</span><span class="kbd">Ctrl A autopilot</span>
    </div>`,
  });
  return `<section style="flex:1;min-width:0;display:flex;flex-direction:column;background:var(--bg)">
  <div style="height:40px;flex:none;display:flex;align-items:center;gap:4px;padding:0 10px;border-bottom:1px solid var(--bd)">
    <span style="display:inline-flex;align-items:center;gap:6px;height:28px;padding:0 10px;border-radius:6px;background:var(--s3);font-weight:500"><span class="mono" style="color:var(--tx3)">1</span>principal<span class="mono" style="font-size:11px;color:var(--tx3)">3</span></span>
    <span style="display:inline-flex;align-items:center;gap:6px;height:28px;padding:0 10px;border-radius:6px;color:var(--tx2)"><span class="mono" style="color:var(--tx3)">2</span>migrações<span class="mono" style="font-size:11px;color:var(--tx3)">1</span></span>
    <span style="display:inline-flex;align-items:center;height:28px;padding:0 10px;color:var(--tx3)" class="mono">3</span>
    <div style="flex:1"></div>
    <span style="display:inline-flex;align-items:center;gap:6px;color:var(--tx3);font-size:12px">${ico('folder')}<span class="mono" style="font-size:11.5px;color:var(--tx2)">~/projetos/loja</span>(projeto)</span>
    <span class="btn" style="height:28px;margin-left:8px">${ico('plus')}CLI</span>
  </div>
  <div style="flex:1;min-height:0;display:grid;grid-template-columns:repeat(2,minmax(0,1fr));grid-template-rows:repeat(2,minmax(0,1fr));gap:10px;padding:10px">
    ${frontend}${backend}
    <div style="grid-column:1 / span 2;display:flex;min-height:0">${agente}</div>
  </div>
</section>`;
}

export function workbench({ decisao = true, decisoes = 2 } = {}) {
  return `<div style="display:flex;flex-direction:column;height:100%">
${cabecalho({ trilha: 'loja', decisoes })}
<div style="flex:1;min-height:0;display:flex">${barraLateral()}${chat({ decisao })}${grade()}</div>
${barraStatus()}
</div>`;
}
