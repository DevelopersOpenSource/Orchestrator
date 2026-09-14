// Central de decisões e auditoria (F2).
import { ico } from './comum.mjs';
import { cabecalho, barraLateral, barraStatus } from './cromo.mjs';

const chip = (tipo, texto) => {
  const c = { ok: ['●', 'var(--ok)', 'var(--ok-bg)'], av: ['○', 'var(--av)', 'var(--av-bg)'], er: ['✖', 'var(--er)', 'var(--er-bg)'] }[tipo];
  return `<span style="display:inline-flex;align-items:center;gap:5px;height:22px;padding:0 8px;border-radius:5px;background:${c[2]};color:${c[1]};font-size:12px;font-weight:500;white-space:nowrap"><span style="font-size:9px">${c[0]}</span>${texto}</span>`;
};

const aba = (nome, n, ativa) =>
  `<span style="display:inline-flex;align-items:center;gap:7px;height:40px;padding:0 2px;margin-right:22px;border-bottom:2px solid ${ativa ? 'var(--tx)' : 'transparent'};color:${ativa ? 'var(--tx)' : 'var(--tx2)'};font-weight:${ativa ? 600 : 400}">${nome}<span class="mono" style="font-size:11px;padding:0 5px;border-radius:4px;background:var(--s3);color:var(--tx2)">${n}</span></span>`;

function pedido({ cli, hora, cmd, regra, sel }) {
  return `<div style="padding:12px 14px;border-radius:8px;border:1px solid ${sel ? 'var(--bd2)' : 'transparent'};background:${sel ? 'var(--s1)' : 'transparent'};display:flex;flex-direction:column;gap:6px">
    <div style="display:flex;align-items:center;gap:8px"><span style="font-weight:600">${cli}</span><span style="color:var(--tx3);font-size:12px">pede confirmação</span><div style="flex:1"></div><span class="mono" style="font-size:11px;color:var(--tx3)">${hora}</span></div>
    <div class="mono trunc" style="font-size:12.5px">${cmd}</div>
    <div style="font-size:12px;color:var(--tx3)">regra: ${regra}</div>
  </div>`;
}

const campo = (rotulo, valor) =>
  `<div style="display:flex;flex-direction:column;gap:3px;min-width:0"><span style="font-size:11.5px;color:var(--tx3)">${rotulo}</span><span class="trunc" style="color:var(--tx)">${valor}</span></div>`;

const AUDITORIA = [
  ['12:05:41', 'av', 'pendente', 'backend', 'git push --force origin main', 'ask-regex · push forçado só com o dono'],
  ['12:04:10', 'er', 'bloqueado', 'frontend', 'rm -rf dist node_modules', 'deny-regex · nunca apagar em massa'],
  ['12:02:05', 'ok', 'permitido', 'Agente #1', 'retrieve_memory · loja · “acessibilidade”', 'consulta registrada, sessão liberada'],
  ['12:02:03', 'er', 'bloqueado', 'Agente #1', 'Bash · npm install', 'sessão ainda não consultou a memória'],
  ['11:58:30', 'ok', 'aprovado', 'backend', 'sqlx migrate run', 'aprovado pelo dono'],
  ['11:52:12', 'er', 'negado', 'backend', 'git reset --hard HEAD~3', 'negado pelo dono'],
];

export function decisoes() {
  const linhas = AUDITORIA.map(([hora, t, dec, quem, acao, motivo]) =>
    `<div style="display:grid;grid-template-columns:84px 118px 110px minmax(0,1.3fr) minmax(0,1fr);align-items:center;gap:12px;height:40px;padding:0 16px;border-top:1px solid var(--bd)">
      <span class="mono" style="font-size:11.5px;color:var(--tx3)">${hora}</span><span>${chip(t, dec)}</span><span class="trunc" style="color:var(--tx2)">${quem}</span>
      <span class="mono trunc" style="font-size:12px">${acao}</span><span class="trunc" style="color:var(--tx2)">${motivo}</span>
    </div>`).join('');

  return `<div style="display:flex;flex-direction:column;height:100%">
${cabecalho({ trilha: 'loja', decisoes: 2 })}
<div style="flex:1;min-height:0;display:flex">${barraLateral()}
<main style="flex:1;min-width:0;display:flex;flex-direction:column;padding:0 28px">
  <div style="display:flex;align-items:flex-end;justify-content:space-between;padding:22px 0 4px">
    <div><div style="font-size:18px;font-weight:600;letter-spacing:-.01em">Decisões e atividade</div><div style="color:var(--tx2);margin-top:2px">O que as CLIs pediram ou tentaram fazer no projeto loja.</div></div>
    <span style="display:inline-flex;gap:8px;align-items:center;color:var(--tx3);font-size:12px"><span class="kbd">a</span> aprova <span class="kbd">d</span> nega <span class="kbd">Esc</span> volta</span>
  </div>
  <div style="display:flex;border-bottom:1px solid var(--bd);margin-top:10px">${aba('Pendentes', 2, true)}${aba('Auditoria', 41, false)}${aba('Ferramentas', 12, false)}</div>
  <div style="display:flex;gap:20px;padding:16px 0;height:392px;flex:none">
    <div style="width:400px;flex:none;display:flex;flex-direction:column;gap:6px">
      ${pedido({ cli: 'backend', hora: '12:05:41', cmd: 'git push --force origin main', regra: 'push forçado só com o dono', sel: true })}
      ${pedido({ cli: 'Agente #1', hora: '12:03:18', cmd: 'npm publish --access public', regra: 'publicar pacote só com o dono' })}
    </div>
    <div style="flex:1;min-width:0;border:1px solid var(--bd);border-radius:10px;background:var(--s1);padding:20px 22px;display:flex;flex-direction:column;gap:16px">
      <div style="display:flex;align-items:center;gap:10px">${chip('av', 'aguardando você')}<span style="color:var(--tx3);font-size:12px">há 38 s</span></div>
      <div style="font-size:15px;font-weight:600">backend quer rodar um comando que pede sua confirmação</div>
      <div class="mono" style="padding:12px 14px;border-radius:6px;background:var(--s2);border:1px solid var(--bd);font-size:13px">git push --force origin main</div>
      <div style="display:grid;grid-template-columns:repeat(3,minmax(0,1fr));gap:14px 20px">
        ${campo('regra', 'push forçado só com o dono')}
        ${campo('tipo', 'segurança · prioridade 10')}
        ${campo('gatilho', '<span class="mono" style="font-size:12px">ask-regex: git push (-f|--force)</span>')}
        ${campo('CLI', 'backend · codex · workspace 1')}
        ${campo('sessão', '<span class="mono" style="font-size:12px">a41c07e2-5b19-4d8e</span>')}
        ${campo('pasta', '<span class="mono" style="font-size:12px">~/projetos/loja</span>')}
      </div>
      <div style="flex:1"></div>
      <div style="display:flex;align-items:center;gap:8px;padding-top:14px;border-top:1px solid var(--bd)">
        <span style="color:var(--tx3);font-size:12px">A CLI fica parada até você decidir.</span><div style="flex:1"></div>
        <span class="btn">${ico('x')}Negar<span class="kbd">d</span></span>
        <span class="btn btn-p">${ico('check')}Aprovar<span class="kbd" style="background:transparent;color:inherit;border-color:rgba(255,255,255,.35)">a</span></span>
      </div>
    </div>
  </div>
  <div style="display:flex;align-items:center;justify-content:space-between;padding:6px 0 10px"><span style="font-weight:600">Atividade recente</span><span style="color:var(--tx3);font-size:12px">log de auditoria, só acrescenta</span></div>
  <div style="border:1px solid var(--bd);border-radius:10px;background:var(--s1);overflow:hidden">
    <div style="display:grid;grid-template-columns:84px 118px 110px minmax(0,1.3fr) minmax(0,1fr);gap:12px;height:34px;align-items:center;padding:0 16px;font-size:11.5px;color:var(--tx3);background:var(--s2)"><span>hora</span><span>decisão</span><span>quem</span><span>ação</span><span>motivo</span></div>
    ${linhas}
  </div>
</main></div>
${barraStatus()}
</div>`;
}
