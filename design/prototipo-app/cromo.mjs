// Moldura do app: cabeçalho, barra lateral e barra de status.
import { ico, estado } from './comum.mjs';

export const MARCA = `<svg viewBox="0 0 20 20" style="width:20px;height:20px;flex:none"><rect x="1" y="1" width="18" height="18" rx="5" fill="var(--tx)"/><circle cx="10" cy="10" r="4.2" fill="none" stroke="var(--bg)" stroke-width="2"/><circle cx="15.2" cy="4.8" r="1.8" fill="var(--ac)"/></svg>`;

export function cabecalho({ trilha = 'loja', decisoes = 2 } = {}) {
  return `<header style="height:44px;display:flex;align-items:center;gap:12px;padding:0 12px 0 14px;border-bottom:1px solid var(--bd);background:var(--s1)">
  <div style="display:flex;align-items:center;gap:8px;width:190px">${MARCA}<span style="font-weight:600;font-size:13.5px;letter-spacing:-.01em">Orchestrator</span></div>
  <div style="display:flex;align-items:center;gap:6px;color:var(--tx2);font-size:13px;min-width:0">
    <span style="color:var(--tx);font-weight:500">${trilha}</span>
  </div>
  <div style="flex:1"></div>
  <div style="display:flex;align-items:center;gap:8px;width:380px;height:30px;padding:0 8px 0 10px;border:1px solid var(--bd2);border-radius:6px;background:var(--bg);color:var(--tx3)">
    ${ico('search')}<span style="flex:1">Buscar ou rodar comando…</span><span class="kbd">Ctrl K</span>
  </div>
  <div style="flex:1"></div>
  <div style="display:flex;align-items:center;gap:6px">
    <span class="btn" style="border-color:${decisoes ? 'var(--av)' : 'var(--bd2)'};color:${decisoes ? 'var(--av)' : 'var(--tx2)'};background:${decisoes ? 'var(--av-bg)' : 'var(--s1)'}">${ico('shield')}${decisoes ? `${decisoes} decisões` : 'Decisões'}<span class="kbd" style="background:transparent">F2</span></span>
    <span class="btn">${ico('nodes')}Memória<span class="kbd">Ctrl ⇧ W</span></span>
    <span class="btn" style="width:30px;padding:0;justify-content:center;color:var(--tx2)">${ico('gear')}</span>
  </div>
</header>`;
}

const rotulo = (t, extra = '') =>
  `<div style="display:flex;align-items:center;justify-content:space-between;padding:14px 10px 6px;font-size:11px;font-weight:600;letter-spacing:.06em;text-transform:uppercase;color:var(--tx3)"><span>${t}</span>${extra}</div>`;

const item = ({ icone, nome, detalhe = '', ativo = false, direita = '' }) =>
  `<div style="display:flex;align-items:center;gap:8px;height:30px;margin:0 6px;padding:0 8px;border-radius:6px;${ativo ? 'background:var(--s3);color:var(--tx)' : 'color:var(--tx2)'}">
    ${icone ? ico(icone, ativo ? 'color:var(--tx)' : '') : ''}<span class="trunc" style="flex:1;${ativo ? 'font-weight:500' : ''}">${nome}${detalhe ? `<span style="color:var(--tx3);font-weight:400"> ${detalhe}</span>` : ''}</span>${direita}</div>`;

const num = (n) => `<span style="font-size:11.5px;color:var(--tx3)" class="mono">${n}</span>`;

export function barraLateral() {
  return `<aside style="width:216px;flex:none;display:flex;flex-direction:column;border-right:1px solid var(--bd);background:var(--s1)">
  ${rotulo('Projetos', ico('plus', 'width:14px;height:14px'))}
  ${item({ icone: 'folder', nome: 'loja', ativo: true })}
  ${item({ icone: 'folder', nome: 'Orchestrator' })}
  ${item({ icone: 'folder', nome: 'site-institucional' })}
  ${rotulo('Workspaces', '<span style="text-transform:none;letter-spacing:0;font-weight:400" class="kbd">Alt ↑↓</span>')}
  ${item({ nome: '<span class="mono" style="color:var(--tx3);margin-right:6px">1</span>principal', ativo: true, direita: num('3') })}
  ${item({ nome: '<span class="mono" style="color:var(--tx3);margin-right:6px">2</span>migrações', direita: num('1') })}
  ${item({ nome: '<span class="mono" style="color:var(--tx3);margin-right:6px">3</span>vazia', direita: '' })}
  ${rotulo('Nesta workspace')}
  ${item({ icone: 'terminal', nome: 'frontend', direita: estado('ac', '') })}
  ${item({ icone: 'terminal', nome: 'backend', direita: estado('ok', '') })}
  ${item({ icone: 'bot', nome: 'Agente #1', direita: '<span class="mono" style="font-size:11px;color:var(--tx3)">3/10</span>' })}
  <div style="flex:1"></div>
  <div style="margin:10px;padding:10px;border:1px solid var(--bd);border-radius:8px;display:flex;flex-direction:column;gap:8px">
    <div style="display:flex;align-items:center;justify-content:space-between"><span style="font-weight:500">Abrir CLI</span><span class="kbd">Ctrl T</span></div>
    <div style="display:flex;flex-wrap:wrap;gap:6px">
      ${['claude', 'codex', 'gemini', 'kimi'].map(c => `<span class="mono" style="font-size:11.5px;padding:2px 7px;border:1px solid var(--bd2);border-radius:5px;color:var(--tx2)">${c}</span>`).join('')}
    </div>
  </div>
</aside>`;
}

export function barraStatus() {
  const sep = '<span style="color:var(--bd2)">·</span>';
  return `<footer style="height:28px;display:flex;align-items:center;gap:10px;padding:0 12px;border-top:1px solid var(--bd);background:var(--s1);font-size:12px;color:var(--tx3)">
  ${estado('ok', 'memória pronta')}<span>semântica + reranker</span>${sep}<span>chroma conectado</span>${sep}<span class="mono" style="font-size:11.5px">API 127.0.0.1:10000</span>
  <div style="flex:1"></div>
  <span>postura: <span style="color:var(--tx2)">autônomo</span></span>${sep}<span class="mono" style="font-size:11.5px">⟳ 18,4k → 3,2k tok · US$ 0,19</span>${sep}<span>manual <span class="kbd">F1</span></span>
</footer>`;
}
