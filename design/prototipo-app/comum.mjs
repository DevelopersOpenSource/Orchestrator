// Peças comuns do protótipo: tokens, ícones e o envelope do artboard.

export const FONTES =
  '<link rel="stylesheet" href="https://fonts.googleapis.com/css2?family=IBM+Plex+Sans:wght@400;500;600&family=JetBrains+Mono:wght@400;500&display=swap">';

// Tokens: base zinco, um acento (azul-aço), estados verde/âmbar/vermelho.
export const TOKENS = `
.t-escuro{--bg:#111113;--s1:#18181b;--s2:#1f1f23;--s3:#27272c;--bd:#2a2a30;--bd2:#3a3a42;
 --tx:#e7e7ea;--tx2:#a1a1aa;--tx3:#71717a;--ac:#6aa9d8;--ac-bg:#1b2a36;--ac-tx:#0d1a24;
 --ok:#4fb67a;--ok-bg:#16271d;--av:#d9a441;--av-bg:#2e2413;--er:#e5534b;--er-bg:#31181a;
 --k-sec:#e5534b;--k-arq:#6b95e8;--k-pra:#3fb0bf;--k-sin:#5fb97a;--k-dec:#b387d9;
 --term:#0e0e10;--scrim:rgba(0,0,0,.55);--sombra:0 12px 32px rgba(0,0,0,.45)}
.t-claro{--bg:#f6f6f7;--s1:#ffffff;--s2:#f1f1f3;--s3:#e9e9ec;--bd:#e4e4e7;--bd2:#d4d4d8;
 --tx:#18181b;--tx2:#52525b;--tx3:#71717a;--ac:#2d73a6;--ac-bg:#e6f0f7;--ac-tx:#ffffff;
 --ok:#1f8a4c;--ok-bg:#e5f4eb;--av:#a86b0c;--av-bg:#fbf0dc;--er:#c93a32;--er-bg:#fbe9e7;
 --k-sec:#c93a32;--k-arq:#3a68c9;--k-pra:#1b8796;--k-sin:#2e8a4e;--k-dec:#8a55b8;
 --term:#fbfbfc;--scrim:rgba(24,24,27,.35);--sombra:0 12px 32px rgba(24,24,27,.16)}
`;

export const BASE = `
*{box-sizing:border-box}
html,body{overflow:hidden}
body{margin:0;font-family:"IBM Plex Sans",system-ui,-apple-system,"Segoe UI",sans-serif;font-size:13px;line-height:1.45;-webkit-font-smoothing:antialiased}
a{color:var(--ac,#2d73a6);text-decoration:none} a:hover{color:var(--tx,#18181b);text-decoration:underline}
.mono{font-family:"JetBrains Mono",ui-monospace,"SFMono-Regular",Menlo,monospace}
.kbd{font-family:"JetBrains Mono",ui-monospace,monospace;font-size:11px;padding:1px 5px;border:1px solid var(--bd2);border-bottom-width:2px;border-radius:4px;color:var(--tx2);background:var(--s1);white-space:nowrap}
.btn{display:inline-flex;align-items:center;gap:6px;height:30px;padding:0 12px;border-radius:6px;border:1px solid var(--bd2);background:var(--s1);color:var(--tx);font:500 13px/1 "IBM Plex Sans",system-ui,sans-serif;white-space:nowrap}
.btn-p{background:var(--ac);border-color:var(--ac);color:var(--ac-tx)}
.ico{width:16px;height:16px;flex:none;stroke:currentColor;fill:none;stroke-width:1.5;stroke-linecap:round;stroke-linejoin:round}
.sep{width:1px;align-self:stretch;background:var(--bd)}
.trunc{white-space:nowrap;overflow:hidden;text-overflow:ellipsis;min-width:0}
`;

const PATHS = {
  folder: '<path d="M2.5 4.5a1 1 0 0 1 1-1h3l1.5 1.5h4.5a1 1 0 0 1 1 1v6a1 1 0 0 1-1 1h-9a1 1 0 0 1-1-1z"/>',
  terminal: '<rect x="2" y="3" width="12" height="10" rx="1.5"/><path d="M5 6.5 7 8.5 5 10.5M8.5 10.5h2.5"/>',
  plus: '<path d="M8 3.5v9M3.5 8h9"/>',
  search: '<circle cx="7" cy="7" r="4"/><path d="m10 10 3 3"/>',
  shield: '<path d="M8 2.5 3.5 4v4c0 2.6 1.9 4.6 4.5 5.5 2.6-.9 4.5-2.9 4.5-5.5V4z"/><path d="M6.2 8 7.5 9.3 10 6.8"/>',
  nodes: '<circle cx="8" cy="8" r="1.6"/><circle cx="3.5" cy="4" r="1.2"/><circle cx="12.5" cy="4" r="1.2"/><circle cx="4" cy="12.5" r="1.2"/><circle cx="12" cy="12" r="1.2"/><path d="m4.5 4.8 2.2 2M11.5 4.8 9.3 6.8M4.9 11.6l1.9-2.4M11.1 11.2 9.3 9.2"/>',
  gear: '<circle cx="8" cy="8" r="2"/><path d="M8 2v1.6M8 12.4V14M2 8h1.6M12.4 8H14M3.8 3.8l1.1 1.1M11.1 11.1l1.1 1.1M3.8 12.2l1.1-1.1M11.1 4.9l1.1-1.1"/>',
  x: '<path d="m4.5 4.5 7 7M11.5 4.5l-7 7"/>',
  check: '<path d="m3.5 8.5 3 3 6-7"/>',
  chevron: '<path d="m6 4 4 4-4 4"/>',
  down: '<path d="m4 6 4 4 4-4"/>',
  send: '<path d="M3 8h9M8.5 4.5 12 8l-3.5 3.5"/>',
  panel: '<rect x="2.5" y="3" width="11" height="10" rx="1.5"/><path d="M6.5 3v10"/>',
  bot: '<rect x="3" y="5" width="10" height="7.5" rx="2"/><path d="M8 5V3M6 8.5v.5M10 8.5v.5"/>',
  globe: '<circle cx="8" cy="8" r="5.5"/><path d="M2.5 8h11M8 2.5c1.6 1.7 2.4 3.5 2.4 5.5S9.6 11.8 8 13.5C6.4 11.8 5.6 10 5.6 8S6.4 4.2 8 2.5"/>',
  expand: '<path d="M9.5 3H13v3.5M6.5 13H3V9.5M13 3 9 7M3 13l4-4"/>',
  pin: '<path d="M6 2.5h4l-.6 4 2.1 2H4.5l2.1-2zM8 8.5V14"/>',
  edit: '<path d="M10.5 3 13 5.5 6 12.5H3.5V10z"/>',
  trash: '<path d="M3.5 4.5h9M6.5 4.5V3h3v1.5M5 4.5l.5 8.5h5l.5-8.5"/>',
  copy: '<rect x="5.5" y="5.5" width="7.5" height="7.5" rx="1.2"/><path d="M10.5 5.5V3.8a.8.8 0 0 0-.8-.8H3.8a.8.8 0 0 0-.8.8v5.9c0 .5.4.8.8.8h1.7"/>',
  clock: '<circle cx="8" cy="8" r="5.5"/><path d="M8 5v3l2 1.5"/>',
  filter: '<path d="M2.5 4h11M4.5 8h7M6.5 12h3"/>',
  menu: '<path d="M3 4.5h10M3 8h10M3 11.5h10"/>',
  bolt: '<path d="M8.8 2 4 9h3.5l-.8 5L12 7H8.5z"/>',
};

export const ico = (nome, estilo = '') =>
  `<svg class="ico" viewBox="0 0 16 16" style="${estilo}">${PATHS[nome]}</svg>`;

// Marca de estado: glifo + cor + texto, nunca só cor.
export const estado = (tipo, texto) => {
  const m = { ok: ['●', 'var(--ok)'], av: ['○', 'var(--av)'], er: ['✖', 'var(--er)'], ac: ['⚙', 'var(--ac)'] }[tipo];
  return `<span style="display:inline-flex;align-items:center;gap:5px;color:${m[1]};font-size:12px;white-space:nowrap"><span style="font-size:10px">${m[0]}</span>${texto}</span>`;
};

export const TIPOS = {
  security: ['segurança', 'var(--k-sec)'],
  architecture: ['arquitetura', 'var(--k-arq)'],
  practice: ['prática', 'var(--k-pra)'],
  syntax: ['sintaxe', 'var(--k-sin)'],
  decision: ['decisão', 'var(--k-dec)'],
};

export const tipo = (k) =>
  `<span style="display:inline-flex;align-items:center;gap:6px;font-size:12px;color:var(--tx2);white-space:nowrap"><span style="width:7px;height:7px;border-radius:2px;background:${TIPOS[k][1]}"></span>${TIPOS[k][0]}</span>`;

// Envelope de artboard. `tema` vira tweak quando informado.
export function artboard({ corpo, tema, extraCss = '', largura, altura }) {
  const props = { $preview: { width: largura, height: altura } };
  let logica = 'class Component extends DCLogic {}';
  let classe = 't-escuro';
  if (tema) {
    props.tema = { editor: 'enum', options: ['escuro', 'claro'], default: tema };
    logica = `class Component extends DCLogic {
  renderVals() {
    const tema = this.props.tema ?? '${tema}';
    return { temaClasse: tema === 'claro' ? 't-claro' : 't-escuro' };
  }
}`;
    classe = '{{temaClasse}}';
  }
  return `<!doctype html>
<html>
<head>
  <meta charset="utf-8">
  <script src="./support.js"></script>
</head>
<body>
<x-dc>
<helmet>
  ${FONTES}
  <style>${TOKENS}${BASE}${extraCss}</style>
</helmet>
<div class="${classe}" style="width:${largura}px;height:${altura}px;overflow:hidden;position:absolute;left:0;top:0;background:var(--bg);color:var(--tx)">
${corpo}
</div>
</x-dc>
<script data-dc-script data-props='${JSON.stringify(props)}'>
${logica}
</script>
</body>
</html>
`;
}
