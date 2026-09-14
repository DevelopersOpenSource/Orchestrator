// Gera os artboards do protótipo: node gerar.mjs (dentro desta pasta).
import { writeFileSync } from 'node:fs';
import { artboard } from './comum.mjs';
import { workbench } from './tela-workbench.mjs';
import { paleta } from './tela-paleta.mjs';
import { decisoes } from './tela-decisoes.mjs';
import { memoria } from './tela-memoria.mjs';
import { globoDesktop, globoCelular } from './tela-globo.mjs';

const telas = [
  { arquivo: 'Main.dc.html', titulo: 'Workbench', corpo: workbench(), tema: 'escuro', w: 1440, h: 900, x: 0, y: 0 },
  { arquivo: 'Paleta.dc.html', titulo: 'Paleta de comandos', corpo: paleta(), tema: 'escuro', w: 1440, h: 900, x: 1540, y: 0 },
  { arquivo: 'Decisoes.dc.html', titulo: 'Decisões e auditoria', corpo: decisoes(), tema: 'claro', w: 1440, h: 900, x: 0, y: 1040 },
  { arquivo: 'Memoria.dc.html', titulo: 'Painel de memória', corpo: memoria(), tema: 'claro', w: 1440, h: 900, x: 1540, y: 1040 },
  { arquivo: 'Globo.dc.html', titulo: 'Página da memória · 127.0.0.1:10000', corpo: globoDesktop(), w: 1440, h: 900, x: 0, y: 2080 },
  { arquivo: 'GloboCelular.dc.html', titulo: 'Página da memória · celular', corpo: globoCelular(), w: 390, h: 844, x: 1540, y: 2080 },
];

for (const t of telas) {
  writeFileSync(t.arquivo, artboard({ corpo: t.corpo, tema: t.tema, largura: t.w, altura: t.h }));
}

const canvas = {
  artboards: telas.map(t => ({ file: t.arquivo, title: t.titulo, x: t.x, y: t.y, w: t.w, h: t.h })),
  annotations: [{
    id: 'direcao', x: 3080, y: 0, w: 340,
    text: 'Direção visual\n\nBase zinco neutra, um acento azul-aço, IBM Plex Sans na interface e JetBrains Mono nos terminais. Bordas de 1 px, raio 6 a 10 px, sombra só no que flutua. Sem vidro, sem gradiente, sem brilho: o brilho existe só no globo, onde é conteúdo.\n\nEstado sempre com glifo, cor e texto: ● pronto · ○ falta argumento · ✖ indisponível.\n\nAs telas do app trocam entre escuro e claro no chip "tema" acima de cada uma.\n\nO globo aqui é estático. No app ele gira devagar, as partículas correm pelos ramos e a câmera voa até o melhor resultado.',
  }, {
    id: 'celular', x: 2030, y: 2080, w: 300,
    text: 'Em tela estreita, o painel de resultados vira uma folha que sobe de baixo. Os filtros ficam atrás do botão no canto.',
  }],
  launch: { view: 'canvas' },
};
writeFileSync('canvas.json', JSON.stringify(canvas, null, 2));
console.log(telas.map(t => t.arquivo).join(' '));
