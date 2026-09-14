// Painel de memória (Ctrl+Shift+W / F4) com o editor aberto.
import { ico, tipo, TIPOS } from './comum.mjs';
import { workbench } from './tela-workbench.mjs';

const MEMORIAS = [
  { k: 'security', p: 10, t: 'nunca apagar em massa', extra: 'deny-regex: \\brm\\s+-rf' },
  { k: 'security', p: 10, t: 'push forçado só com o dono', extra: 'ask-regex: git push (-f|--force)' },
  { k: 'practice', p: 9, t: 'Pull requests pequenos, um assunto por vez', fixa: true },
  { k: 'architecture', p: 3, t: 'Banco de dados é PostgreSQL via sqlx', sel: true },
  { k: 'decision', p: 6, t: 'Valores monetários em centavos, sempre inteiros' },
];

const aba = (nome, n, ativa) =>
  `<span style="display:inline-flex;align-items:center;gap:7px;height:40px;margin-right:22px;border-bottom:2px solid ${ativa ? 'var(--tx)' : 'transparent'};color:${ativa ? 'var(--tx)' : 'var(--tx2)'};font-weight:${ativa ? 600 : 400}">${nome}<span class="mono" style="font-size:11px;padding:0 5px;border-radius:4px;background:var(--s3);color:var(--tx2)">${n}</span></span>`;

const rotulo = (t, dica = '') =>
  `<div style="display:flex;justify-content:space-between;align-items:baseline;margin-bottom:6px"><span style="font-size:12px;font-weight:500;color:var(--tx2)">${t}</span><span style="font-size:11.5px;color:var(--tx3)">${dica}</span></div>`;

export function memoria() {
  const lista = MEMORIAS.map(m => `<div style="display:flex;flex-direction:column;gap:5px;padding:11px 14px;border-radius:8px;${m.sel ? 'background:var(--s2);border:1px solid var(--bd2)' : 'border:1px solid transparent'}">
    <div style="display:flex;align-items:center;gap:8px"><span class="trunc" style="flex:1;font-weight:500">${m.t}</span>${m.fixa ? `<span style="display:inline-flex;align-items:center;gap:4px;font-size:11px;font-weight:500;color:var(--ac);padding:1px 6px;border-radius:4px;background:var(--ac-bg)">${ico('pin', 'width:12px;height:12px')}fixa</span>` : ''}</div>
    <div style="display:flex;align-items:center;gap:10px">${tipo(m.k)}<span class="mono" style="font-size:11px;color:var(--tx3)">p${m.p}</span><span style="font-size:12px;color:var(--tx3)">dono</span>${m.extra ? `<span class="mono trunc" style="font-size:11px;color:var(--tx3)">${m.extra}</span>` : ''}</div>
  </div>`).join('');

  const segmento = Object.entries(TIPOS).map(([k, [nome, cor]]) => {
    const on = k === 'architecture';
    return `<span style="flex:1;display:inline-flex;align-items:center;justify-content:center;gap:6px;height:30px;border-radius:5px;font-size:12.5px;${on ? 'background:var(--s1);box-shadow:0 0 0 1px var(--bd2);color:var(--tx);font-weight:500' : 'color:var(--tx2)'}"><span style="width:7px;height:7px;border-radius:2px;background:${cor}"></span>${nome}</span>`;
  }).join('');

  const entrada = (conteudo, extra = '') =>
    `<div style="min-height:34px;display:flex;align-items:center;padding:0 10px;border:1px solid var(--bd2);border-radius:6px;background:var(--s1);${extra}">${conteudo}</div>`;

  return `${workbench({ decisao: true, decisoes: 2 })}
<div style="position:absolute;inset:0;background:var(--scrim)"></div>
<div style="position:absolute;left:100px;top:56px;width:1240px;height:788px;display:flex;flex-direction:column;border:1px solid var(--bd2);border-radius:12px;background:var(--bg);box-shadow:var(--sombra);overflow:hidden">
  <div style="height:56px;flex:none;display:flex;align-items:center;gap:10px;padding:0 14px 0 20px;background:var(--s1);border-bottom:1px solid var(--bd)">
    ${ico('nodes', 'width:18px;height:18px')}<span style="font-size:15px;font-weight:600">Memória</span><span style="color:var(--tx3)">loja</span>
    <div style="flex:1"></div>
    <div style="display:flex;align-items:center;gap:8px;width:320px;height:32px;padding:0 8px 0 10px;border:1px solid var(--bd2);border-radius:6px;background:var(--bg);color:var(--tx3)">${ico('search')}<span style="flex:1">Buscar por significado</span><span class="kbd">/</span></div>
    <span class="btn">${ico('globe')}Globo</span>
    <span class="btn btn-p">${ico('plus')}Nova<span class="kbd" style="background:transparent;color:inherit;border-color:rgba(255,255,255,.35)">n</span></span>
    <span class="sep" style="margin:10px 2px"></span>
    <span style="display:flex;align-items:center;gap:6px;color:var(--tx3)"><span class="kbd">Ctrl ⇧ W</span>${ico('x')}</span>
  </div>
  <div style="display:flex;padding:0 20px;border-bottom:1px solid var(--bd);background:var(--s1)">${aba('Global, todo projeto', 3, false)}${aba('Projeto: loja', 5, true)}${aba('IAs do projeto', 1, false)}</div>
  <div style="flex:1;min-height:0;display:flex">
    <div style="width:500px;flex:none;padding:12px;display:flex;flex-direction:column;gap:4px;border-right:1px solid var(--bd)">${lista}
      <div style="flex:1"></div>
      <div style="padding:10px 14px;font-size:12px;color:var(--tx3);text-wrap:pretty">Regras de segurança são impostas pelo hook. Prioridade 9 ou mais faz de uma prática uma regra fixa, lembrada em todo prompt.</div>
    </div>
    <div style="flex:1;min-width:0;display:flex;flex-direction:column;padding:20px 24px;gap:18px">
      <div style="display:flex;align-items:center;gap:10px"><span style="font-size:15px;font-weight:600">Editando</span><span style="color:var(--tx3)">este projeto · criada pelo dono</span><div style="flex:1"></div><span style="font-size:12px;color:var(--tx3)">indexada · e5-small</span></div>
      <div>${rotulo('Tipo', '← → troca')}<div style="display:flex;gap:2px;padding:3px;border-radius:7px;background:var(--s3)">${segmento}</div></div>
      <div style="display:grid;grid-template-columns:minmax(0,1fr) 150px;gap:14px">
        <div>${rotulo('Título')}${entrada('Banco de dados é PostgreSQL via sqlx<span style="width:1.5px;height:16px;background:var(--ac);margin-left:1px"></span>')}</div>
        <div>${rotulo('Prioridade', '0 a 10')}${entrada('<span style="flex:1" class="mono">3</span><span style="color:var(--tx3);display:flex;gap:8px">−<span>+</span></span>')}</div>
      </div>
      <div style="flex:1;display:flex;flex-direction:column">${rotulo('Corpo', 'Enter quebra linha')}
        <div style="flex:1;padding:10px 12px;border:1px solid var(--bd2);border-radius:6px;background:var(--s1);line-height:1.6">O backend usa PostgreSQL com a crate sqlx e migrações na pasta <span class="mono" style="font-size:12px">migrations/</span>.<br>Consultas sempre com <span class="mono" style="font-size:12px">query!</span> verificada em compilação.</div>
      </div>
      <div style="display:flex;align-items:center;gap:8px">
        <span class="btn" style="border-color:transparent;color:var(--er);padding:0 8px">${ico('trash')}Apagar</span>
        <div style="flex:1"></div>
        <span class="btn">Cancelar<span class="kbd">Esc</span></span>
        <span class="btn btn-p">Salvar<span class="kbd" style="background:transparent;color:inherit;border-color:rgba(255,255,255,.35)">Ctrl Enter</span></span>
      </div>
    </div>
  </div>
  <div style="height:34px;flex:none;display:flex;align-items:center;gap:14px;padding:0 20px;border-top:1px solid var(--bd);background:var(--s1);font-size:12px;color:var(--tx3)">
    <span><span class="kbd">Tab</span> aba</span><span><span class="kbd">↑↓</span> escolhe</span><span><span class="kbd">n</span> nova</span><span><span class="kbd">e</span> edita</span><span><span class="kbd">d</span> apaga</span><span><span class="kbd">/</span> busca</span>
  </div>
</div>`;
}
