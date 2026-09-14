// Página da memória servida pelo memoryd em 127.0.0.1:10000.
import { ico, tipo } from './comum.mjs';
import { MARCA } from './cromo.mjs';
import { globoSvg } from './globo.mjs';

const RESULTADOS = [
  { t: 'Banco de dados é PostgreSQL via sqlx', s: 0.325, k: 'architecture', o: 'dono',
    corpo: 'O backend usa PostgreSQL com a crate sqlx e migrações na pasta migrations/.' },
  { t: 'Valores monetários em centavos, sempre inteiros', s: 0.142, k: 'decision', o: 'dono' },
  { t: 'Frontend usa Vite + React', s: 0.119, k: 'decision', o: 'IA: frontend' },
];

const barra = (s, largura = 64) =>
  `<span style="display:inline-flex;align-items:center;gap:8px"><span style="width:${largura}px;height:4px;border-radius:2px;background:var(--s3);overflow:hidden"><span style="display:block;height:100%;width:${Math.round(s / 0.4 * 100)}%;background:var(--ac)"></span></span><span class="mono" style="font-size:11.5px;color:var(--tx2)">${s.toFixed(3).replace('.', ',')}</span></span>`;

const segmentado = (itens, altura = 30) =>
  `<div style="display:flex;gap:2px;padding:3px;border-radius:7px;background:var(--s2);border:1px solid var(--bd)">${itens.map((n, i) =>
    `<span style="flex:1;display:inline-flex;align-items:center;justify-content:center;gap:6px;height:${altura}px;padding:0 14px;border-radius:5px;${i === 0 ? 'background:var(--s3);color:var(--tx);font-weight:500' : 'color:var(--tx2)'}">${n}</span>`).join('')}</div>`;

const filtro = (t) =>
  `<span style="display:inline-flex;align-items:center;gap:6px;height:30px;padding:0 10px;border:1px solid var(--bd2);border-radius:6px;background:var(--s1);color:var(--tx2);white-space:nowrap">${t}${ico('down', 'width:12px;height:12px')}</span>`;

const legenda = () => `<div style="display:flex;flex-wrap:wrap;gap:6px 16px">
  ${['security', 'architecture', 'practice', 'syntax', 'decision'].map(tipo).join('')}
  <span style="display:inline-flex;align-items:center;gap:6px;font-size:12px;color:var(--tx2)"><span style="width:9px;height:9px;border-radius:50%;border:1.5px solid var(--tx)"></span>polo (global ou projeto)</span>
</div>`;

const ROTULOS = RESULTADOS.map(r => ({ rotulo: r.t }));

export function globoDesktop() {
  const cards = RESULTADOS.map((r, n) => `<div style="padding:14px 16px;border-radius:8px;border:1px solid ${n === 0 ? 'var(--ac)' : 'var(--bd)'};background:var(--s1);display:flex;flex-direction:column;gap:8px">
    <div style="display:flex;align-items:flex-start;gap:10px"><span class="mono" style="font-size:11.5px;color:var(--tx3);padding-top:1px">${n + 1}</span><span style="flex:1;font-weight:500;text-wrap:pretty">${r.t}</span></div>
    <div style="display:flex;align-items:center;gap:12px;padding-left:20px">${barra(r.s)}${tipo(r.k)}<span style="font-size:12px;color:var(--tx3)">loja · ${r.o}</span></div>
    ${r.corpo ? `<div style="padding-left:20px;color:var(--tx2);text-wrap:pretty">${r.corpo}</div>` : ''}
  </div>`).join('');

  return `<div style="display:flex;flex-direction:column;height:100%">
  <header style="height:56px;flex:none;display:flex;align-items:center;gap:12px;padding:0 20px;border-bottom:1px solid var(--bd);background:var(--s1)">
    ${MARCA}<span style="font-weight:600;font-size:14px">Memória</span><span class="mono" style="font-size:12px;color:var(--tx3)">127.0.0.1:10000</span>
    <div style="flex:1"></div>
    <div style="width:280px">${segmentado([`${ico('globe')}Globo`, 'Lista', 'API'], 26)}</div>
    <div style="flex:1"></div>
    <span style="font-size:12px;color:var(--tx3)">somente leitura</span>
    <span class="btn">Colar token para editar</span>
  </header>
  <div style="flex:1;min-height:0;display:flex">
    <div style="flex:1;min-width:0;position:relative;overflow:hidden">
      <div style="position:absolute;left:28px;right:28px;top:22px;display:flex;flex-direction:column;gap:10px;z-index:1">
        <div style="display:flex;align-items:center;gap:10px;height:46px;padding:0 10px 0 14px;border:1px solid var(--bd2);border-radius:8px;background:var(--s1)">
          ${ico('search', 'color:var(--tx3)')}<span style="flex:1;font-size:15px">qual SGBD a gente usa no backend?</span>
          <span style="font-size:12px;color:var(--tx3)">reordenado com reranker</span><span class="kbd">Enter</span>
        </div>
        <div style="display:flex;align-items:center;gap:8px">${filtro('Projeto: loja')}${filtro('Tipo: todos')}${filtro('Origem: todas')}<div style="flex:1"></div><span style="font-size:12px;color:var(--tx3)">150 memórias · 4 polos</span></div>
      </div>
      <div style="position:absolute;left:50%;top:108px;width:740px;height:740px;margin-left:-370px">${globoSvg({ tamanho: 800, destaques: ROTULOS })}</div>
      <div style="position:absolute;left:28px;right:28px;bottom:18px;display:flex;align-items:flex-end;justify-content:space-between;gap:20px">
        ${legenda()}
        <span style="font-size:12px;color:var(--tx3);white-space:nowrap">arraste gira · roda aproxima · clique abre</span>
      </div>
    </div>
    <aside style="width:420px;flex:none;display:flex;flex-direction:column;border-left:1px solid var(--bd);background:var(--bg)">
      <div style="padding:18px 20px 12px;display:flex;align-items:baseline;justify-content:space-between"><span style="font-size:15px;font-weight:600">3 resultados</span><span class="mono" style="font-size:11.5px;color:var(--tx3)">164 ms · vetores + reranker</span></div>
      <div style="padding:0 16px;display:flex;flex-direction:column;gap:8px">${cards}</div>
      <div style="padding:12px 20px;font-size:12px;color:var(--tx3);text-wrap:pretty">2 candidatos ficaram abaixo do corte de relevância e não aparecem.</div>
      <div style="flex:1"></div>
      <div style="margin:16px;border:1px solid var(--bd);border-radius:8px;background:var(--s1);overflow:hidden">
        <div style="display:flex;align-items:center;justify-content:space-between;padding:8px 12px;border-bottom:1px solid var(--bd);font-size:12px;color:var(--tx2)"><span>A mesma busca pela API</span><span style="display:flex;color:var(--tx3)">${ico('copy')}</span></div>
        <div class="mono" style="padding:10px 12px;font-size:11.5px;line-height:1.7;color:var(--tx2);word-break:break-all"><span style="color:var(--ac)">GET</span> /api/search?q=qual+SGBD+a+gente+usa+no+backend%3F&amp;project=loja&amp;limit=5</div>
      </div>
    </aside>
  </div>
</div>`;
}

export function globoCelular() {
  const cards = RESULTADOS.map((r, n) => `<div style="min-height:82px;padding:12px 14px;border-radius:8px;border:1px solid ${n === 0 ? 'var(--ac)' : 'var(--bd)'};background:var(--s1);display:flex;flex-direction:column;gap:8px">
    <span class="trunc" style="font-weight:500;font-size:14px">${r.t}</span>
    <div style="display:flex;align-items:center;gap:10px">${barra(r.s, 44)}${tipo(r.k)}<span class="trunc" style="font-size:12px;color:var(--tx3)">${r.o}</span></div>
  </div>`).join('');

  return `<div style="display:flex;flex-direction:column;height:100%;position:relative">
  <header style="height:56px;flex:none;display:flex;align-items:center;gap:10px;padding:0 6px 0 16px;border-bottom:1px solid var(--bd);background:var(--s1)">
    ${MARCA}<span style="font-weight:600;font-size:15px">Memória</span><div style="flex:1"></div>
    <span style="width:44px;height:44px;display:flex;align-items:center;justify-content:center;color:var(--tx2)">${ico('filter', 'width:20px;height:20px')}</span>
  </header>
  <div style="padding:12px 16px 0;display:flex;flex-direction:column;gap:10px">
    <div style="display:flex;align-items:center;gap:10px;height:46px;padding:0 14px;border:1px solid var(--bd2);border-radius:8px;background:var(--s1)">${ico('search', 'color:var(--tx3)')}<span class="trunc" style="flex:1;font-size:15px">qual SGBD a gente usa no backend?</span></div>
    ${segmentado(['Globo', 'Lista', 'API'], 38)}
  </div>
  <div style="flex:none;height:330px;display:flex;justify-content:center;overflow:hidden"><div style="width:380px;height:380px;margin-top:-20px">${globoSvg({ tamanho: 800, texto: 1.9, rotulos: false, destaques: ROTULOS.map(() => ({})) })}</div></div>
  <div style="position:absolute;left:0;right:0;bottom:0;height:392px;border-top:1px solid var(--bd2);border-radius:14px 14px 0 0;background:var(--bg);display:flex;flex-direction:column">
    <div style="height:22px;display:flex;align-items:center;justify-content:center"><span style="width:36px;height:4px;border-radius:2px;background:var(--bd2)"></span></div>
    <div style="display:flex;align-items:baseline;justify-content:space-between;padding:2px 16px 10px"><span style="font-size:15px;font-weight:600">3 resultados</span><span class="mono" style="font-size:11.5px;color:var(--tx3)">164 ms</span></div>
    <div style="padding:0 16px;display:flex;flex-direction:column;gap:8px">${cards}</div>
  </div>
</div>`;
}
