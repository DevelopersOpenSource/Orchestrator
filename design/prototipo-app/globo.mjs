// Gera o globo de memória como SVG estático (o real é 3d-force-graph).
// Pontos numa esfera de Fibonacci, polos por projeto, ramos até o polo,
// vizinhança semântica fina e partículas nos ramos dos resultados.

const COR_TIPO = {
  security: '#e5534b', architecture: '#6b95e8', practice: '#3fb0bf',
  syntax: '#5fb97a', decision: '#b387d9',
};
const ACENTO = '#6aa9d8';

function rng(seed) {
  let s = seed >>> 0;
  return () => ((s = (s * 1664525 + 1013904223) >>> 0) / 4294967296);
}

export function globoSvg({ tamanho = 800, texto = 1, rotulos = true, destaques = [] } = {}) {
  const N = 150;
  const R = tamanho * 0.36;
  const cx = tamanho / 2, cy = tamanho / 2;
  const rand = rng(7);
  const ax = -0.42, ay = 0.55; // inclinação da câmera

  const pts = [];
  const ouro = Math.PI * (3 - Math.sqrt(5));
  for (let i = 0; i < N; i++) {
    const y = 1 - (i / (N - 1)) * 2;
    const r = Math.sqrt(1 - y * y);
    const t = ouro * i;
    const j = 0.06;
    let p = [Math.cos(t) * r + (rand() - .5) * j, y + (rand() - .5) * j, Math.sin(t) * r + (rand() - .5) * j];
    const n = Math.hypot(...p); p = p.map(v => v / n);
    // rotação Y depois X
    let [x1, y1, z1] = [p[0] * Math.cos(ay) + p[2] * Math.sin(ay), p[1], -p[0] * Math.sin(ay) + p[2] * Math.cos(ay)];
    const y2 = y1 * Math.cos(ax) - z1 * Math.sin(ax);
    const z2 = y1 * Math.sin(ax) + z1 * Math.cos(ax);
    const u = rand();
    const kind = u < .12 ? 'security' : u < .36 ? 'architecture' : u < .6 ? 'practice' : u < .78 ? 'syntax' : 'decision';
    pts.push({ p, x: cx + x1 * R, y: cy + y2 * R, z: z2, kind });
  }

  const d3 = (a, b) => Math.hypot(a.p[0] - b.p[0], a.p[1] - b.p[1], a.p[2] - b.p[2]);
  // Polos espalhados: "loja" é o mais de frente; os outros, o mais longe
  // possível dos já escolhidos (entre os pontos não muito ao fundo).
  const nomes = ['loja', 'Global', 'Orchestrator', 'site-institucional'];
  const frente = [...pts.keys()].sort((i, j) => pts[j].z - pts[i].z);
  const escolhidos = [frente[4]];
  while (escolhidos.length < nomes.length) {
    const cand = frente.filter(i => pts[i].z > -0.1 && !escolhidos.includes(i));
    escolhidos.push(cand.reduce((m, i) => {
      const dm = Math.min(...escolhidos.map(e => d3(pts[m], pts[e])));
      const di = Math.min(...escolhidos.map(e => d3(pts[i], pts[e])));
      return di > dm ? i : m;
    }));
  }
  const polos = escolhidos.map((i, n) => ({ i, nome: nomes[n] }));
  const ehPolo = new Map(polos.map(h => [h.i, h]));
  pts.forEach((pt, i) => {
    if (ehPolo.has(i)) return;
    pt.polo = polos.reduce((m, h) => (d3(pt, pts[h.i]) < d3(pt, pts[m.i]) ? h : m)).i;
  });
  // Resultados da busca: nós do polo "loja" mais de frente, rótulo para dentro.
  // Espaçados na vertical para os rótulos não se cobrirem.
  const daLoja = frente.filter(i => pts[i].polo === polos[0].i && pts[i].z > 0.1);
  const centroLoja = pts[polos[0].i];
  const escolhidosDestaque = [];
  for (const i of daLoja) {
    if (escolhidosDestaque.length >= destaques.length) break;
    const a = pts[i];
    if (Math.hypot(a.x - centroLoja.x, a.y - centroLoja.y) < 60) continue;
    if (escolhidosDestaque.every(j => Math.abs(pts[j].y - a.y) > 56)) escolhidosDestaque.push(i);
  }
  destaques = destaques.map((d, n) => {
    const i = escolhidosDestaque[n] ?? daLoja[n];
    return { ...d, i, lado: pts[i].x > cx ? 'esq' : 'dir' };
  });

  const alpha = z => (0.22 + 0.78 * (z + 1) / 2).toFixed(2);
  const partes = [];
  const f = v => v.toFixed(1);

  partes.push(`<circle cx="${cx}" cy="${cy}" r="${R * 1.02}" fill="none" stroke="rgba(161,161,170,.10)" stroke-width="1"/>`);
  partes.push(`<ellipse cx="${cx}" cy="${cy}" rx="${R * 1.02}" ry="${R * 0.36}" fill="none" stroke="rgba(161,161,170,.06)" stroke-width="1" transform="rotate(-14 ${cx} ${cy})"/>`);

  // vizinhança semântica (2 mais próximos)
  const ligadas = new Set();
  pts.forEach((a, i) => {
    pts.map((b, j) => [d3(a, b), j]).filter(([, j]) => j !== i).sort((m, n) => m[0] - n[0]).slice(0, 2)
      .forEach(([d, j]) => {
        const k = i < j ? `${i}-${j}` : `${j}-${i}`;
        if (d > .34 || ligadas.has(k)) return;
        ligadas.add(k);
        const b = pts[j];
        const op = (0.05 + 0.13 * ((a.z + b.z) / 2 + 1) / 2).toFixed(3);
        partes.push(`<line x1="${f(a.x)}" y1="${f(a.y)}" x2="${f(b.x)}" y2="${f(b.y)}" stroke="rgba(161,161,170,${op})" stroke-width="0.8"/>`);
      });
  });

  // ramos memória → polo
  pts.forEach((a) => {
    if (a.polo === undefined) return;
    const h = pts[a.polo];
    const op = (0.08 + 0.2 * ((a.z + h.z) / 2 + 1) / 2).toFixed(3);
    partes.push(`<line x1="${f(a.x)}" y1="${f(a.y)}" x2="${f(h.x)}" y2="${f(h.y)}" stroke="rgba(190,190,200,${op})" stroke-width="0.9"/>`);
  });

  // ramos acesos dos resultados + partículas
  destaques.forEach((d, n) => {
    const a = pts[d.i], h = pts[a.polo];
    const caminho = `M${f(h.x)} ${f(h.y)} L${f(a.x)} ${f(a.y)}`;
    partes.push(`<path d="${caminho}" stroke="${ACENTO}" stroke-opacity=".7" stroke-width="1.4" fill="none"/>`);
    for (let k = 0; k < 2; k++) {
      partes.push(`<circle r="${2 * texto}" fill="#cfe6f7"><animateMotion dur="${2.2 + n * .3}s" begin="${k * 1.1}s" repeatCount="indefinite" path="${caminho}"/></circle>`);
    }
  });

  // nós, de trás para frente
  [...pts.keys()].sort((i, j) => pts[i].z - pts[j].z).forEach((i) => {
    const a = pts[i];
    if (ehPolo.has(i)) return;
    const r = (1.6 + 1.6 * (a.z + 1) / 2) * texto;
    partes.push(`<circle cx="${f(a.x)}" cy="${f(a.y)}" r="${r.toFixed(2)}" fill="${COR_TIPO[a.kind]}" fill-opacity="${alpha(a.z)}"/>`);
  });

  polos.forEach((h) => {
    const a = pts[h.i];
    partes.push(`<circle cx="${f(a.x)}" cy="${f(a.y)}" r="${9 * texto}" fill="#18181b" stroke="#e7e7ea" stroke-opacity="${alpha(a.z)}" stroke-width="${1.5 * texto}"/>`);
    partes.push(`<circle cx="${f(a.x)}" cy="${f(a.y)}" r="${3.2 * texto}" fill="#e7e7ea" fill-opacity="${alpha(a.z)}"/>`);
    if (rotulos) partes.push(`<text x="${f(a.x + 14 * texto)}" y="${f(a.y + 4 * texto)}" fill="#a1a1aa" fill-opacity="${alpha(a.z)}" font-size="${11 * texto}" font-family="IBM Plex Sans, system-ui, sans-serif" font-weight="500">${h.nome}</text>`);
  });

  destaques.forEach((d) => {
    const a = pts[d.i];
    partes.push(`<circle cx="${f(a.x)}" cy="${f(a.y)}" r="${14 * texto}" fill="${ACENTO}" fill-opacity=".16" filter="url(#brilho)"/>`);
    partes.push(`<circle cx="${f(a.x)}" cy="${f(a.y)}" r="${5 * texto}" fill="#e7f2fb"/>`);
    partes.push(`<circle cx="${f(a.x)}" cy="${f(a.y)}" r="${9 * texto}" fill="none" stroke="${ACENTO}" stroke-width="${1.5 * texto}"/>`);
    if (rotulos && d.rotulo) {
      const w = d.rotulo.length * 6.3 * texto + 16 * texto;
      const lx = d.lado === 'esq' ? a.x - 16 * texto - w : a.x + 16 * texto;
      partes.push(`<rect x="${f(lx)}" y="${f(a.y - 11 * texto)}" width="${f(w)}" height="${f(22 * texto)}" rx="${4 * texto}" fill="#18181b" stroke="#3a3a42"/>`);
      partes.push(`<text x="${f(lx + 8 * texto)}" y="${f(a.y + 4 * texto)}" fill="#e7e7ea" font-size="${11.5 * texto}" font-family="IBM Plex Sans, system-ui, sans-serif">${d.rotulo}</text>`);
    }
  });

  return `<svg viewBox="0 0 ${tamanho} ${tamanho}" width="100%" height="100%" style="display:block" aria-label="Globo de memórias">
<defs><filter id="brilho" x="-100%" y="-100%" width="300%" height="300%"><feGaussianBlur stdDeviation="${4 * texto}"/></filter>
<radialGradient id="halo" cx="50%" cy="50%" r="50%"><stop offset="0%" stop-color="#1c2530" stop-opacity=".55"/><stop offset="100%" stop-color="#111113" stop-opacity="0"/></radialGradient></defs>
<circle cx="${cx}" cy="${cy}" r="${R * 1.35}" fill="url(#halo)"/>
${partes.join('\n')}
</svg>`;
}
