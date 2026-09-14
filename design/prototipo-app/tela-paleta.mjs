// Paleta de comandos (Ctrl K ou / no chat) sobre o workbench.
import { workbench } from './tela-workbench.mjs';

// Mesmos comandos e estados de crates/tui/src/palette.rs, no contexto:
// 0 decisões pendentes, 3 projetos, 3 de 8 cards, avisos ligados.
const COMANDOS = [
  ['/cli <nome> [comando]', 'abre uma CLI real nomeada nesta workspace', 'av', 'precisa do nome da CLI'],
  ['/agente <tarefa>', 'agente headless que trabalha sozinho num card', 'av', 'precisa da tarefa'],
  ['/modelo [nome]', 'modelo do chat (sem nome abre o seletor)', 'ok', ''],
  ['/pasta <caminho|->', 'pasta desta workspace (- volta à do projeto)', 'ok', ''],
  ['/projeto [nome]', 'troca de projeto (restaura a conversa dele)', 'ok', ''],
  ['/novo-projeto <nome> [caminho]', 'cria um projeto novo e passa a trabalhar nele', 'av', 'precisa do nome'],
  ['/nova', 'zera a conversa desta workspace', 'ok', ''],
  ['/aprovar [id]', 'aprova a decisão pendente', 'er', 'nenhuma decisão pendente agora'],
  ['/negar [id]', 'nega a decisão pendente', 'er', 'nenhuma decisão pendente agora'],
  ['/auto [on|off]', 'avisar o orquestrador quando uma CLI concluir', 'av', 'agora: avisos ligados'],
  ['/caps', 'o que a build do claude suporta', 'ok', ''],
  ['/ajuda', 'abre o manual completo', 'ok', ''],
];

const GLIFO = { ok: ['●', 'var(--ok)'], av: ['○', 'var(--av)'], er: ['✖', 'var(--er)'] };

export function paleta() {
  const linhas = COMANDOS.map(([uso, sobre, e, motivo], i) => {
    const [g, cor] = GLIFO[e];
    const sel = i === 0;
    const [nome, ...resto] = uso.split(' ');
    const args = resto.map(a => a.replace(/</g, '&lt;').replace(/>/g, '&gt;'));
    return `<div style="display:flex;align-items:center;gap:10px;height:38px;padding:0 14px;border-radius:6px;margin:0 6px;${sel ? 'background:var(--s3)' : ''}">
      <span style="width:14px;text-align:center;font-size:11px;color:${cor}">${g}</span>
      <span class="mono" style="width:236px;flex:none;font-size:12.5px;color:${e === 'er' ? 'var(--tx3)' : 'var(--tx)'}">${nome}<span style="color:var(--tx3)"> ${args.join(' ')}</span></span>
      <span class="trunc" style="flex:1;color:${e === 'er' ? 'var(--tx3)' : 'var(--tx2)'}">${sobre}</span>
      ${motivo ? `<span style="font-size:12px;color:${cor};white-space:nowrap">${motivo}</span>` : ''}
    </div>`;
  }).join('');

  return `${workbench({ decisao: false, decisoes: 0 })}
<div style="position:absolute;inset:0;background:var(--scrim)"></div>
<div style="position:absolute;left:50%;top:92px;width:760px;margin-left:-380px;border:1px solid var(--bd2);border-radius:10px;background:var(--s1);box-shadow:var(--sombra);overflow:hidden">
  <div style="height:54px;display:flex;align-items:center;gap:10px;padding:0 18px;border-bottom:1px solid var(--bd)">
    <span class="mono" style="font-size:16px;color:var(--tx)">/</span><span style="width:1.5px;height:20px;background:var(--ac);margin-left:-8px"></span>
    <span style="flex:1;color:var(--tx3);font-size:14px">comando, projeto ou memória</span>
    <span style="font-size:12px;color:var(--tx3)">12 comandos</span>
  </div>
  <div style="padding:6px 0">${linhas}</div>
  <div style="display:flex;align-items:center;gap:14px;height:42px;padding:0 18px;border-top:1px solid var(--bd);background:var(--s2);font-size:12px;color:var(--tx3)">
    <span class="mono" style="color:var(--tx2)">/cli &lt;nome&gt; [comando]</span><span style="color:var(--av)">precisa do nome da CLI</span>
    <div style="flex:1"></div>
    <span><span class="kbd">↑↓</span> escolhe</span><span><span class="kbd">Tab</span> completa</span><span><span class="kbd">Enter</span> executa</span><span><span class="kbd">Esc</span> fecha</span>
  </div>
</div>`;
}
