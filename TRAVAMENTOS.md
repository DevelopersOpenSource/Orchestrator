# Onde o Orchestrator trava — caderno de observação

Anotações de uso real, para implementar depois. Cada item diz o que
aconteceu, o que já foi feito a respeito, e o que falta.

## Resolvidos durante a observação

| # | Travamento | Correção |
|---|---|---|
| 1 | CLI aberta pelo orquestrador travava num pedido de permissão que ninguém responderia | `cli_envs(managed)` exporta `ORCHESTRATOR_AUTONOMOUS` só para CLIs gerenciadas; o hook vira a autoridade |
| 2 | Prompt sumia quando a CLI ainda estava montando a interface | Estado `Delivering`: escreve, confirma na tela, só então submete; reenvia limpando a caixa e avisa se desistir |
| 3 | Confirmação procurava o começo do prompt, que rola para fora da caixa | Assinatura passou a ser a CAUDA; e "a tela mudou" também confirma |
| 4 | Gate bloqueava o trabalho porque o PROMPT citava um comando destrutivo | `actionable_input` ignora campos de texto livre nas tools do orquestrador |
| 5 | Não dava para responder menu/questionário (via o texto, não a seleção) | `cli_menu`/`cli_choose`/`cli_key` lendo atributos do vt100 |
| 6 | Escapes de bracketed paste entravam como texto em campo simples | Só usa bracketed paste quando a aplicação habilita o modo |
| 7 | MCP do projeto desabilitado por um Enter no prompt de confiança | `enable_mcp_server` remove a marca no setup |
| 8 | Estado de CLIs mortas sobrevivia no banco e confundia o orquestrador | `clear_cli_states` ao abrir a TUI |
| 9 | "Testei na sandbox" não tinha como ser verificado | `tool_log` + aba Ferramentas no F2 |
| 10 | `/work` somente-leitura matava qualquer app que gravasse (~41s de contorno por teste) | Overlay do podman (`:O`): escrita livre DENTRO da sandbox, descartada com o container, pasta do host intocada |
| 11 | `ui_exec` exigia um `ui_open` antes, invertendo a ordem natural do teste | `abre_sozinha` deixa `ui_exec` subir a sandbox sob demanda; e uma sandbox já de pé é readotada em vez de recusada |
| 12 | Emoji da interface aparecia como quadrado no screenshot da sandbox | `fonts-noto-color-emoji` (~11 MB) na imagem — verificado abrindo uma página com ✅ ❌ ⚠️ 🔥 e lendo o PNG |
| 13 | Processo iniciado por uma CLI sobrevivia ao fechamento do card | `kill()` sinaliza cada grupo com processo vivo na SESSÃO da CLI (SIGHUP, prazo curto, SIGKILL no que sobrou) |

## T1 e T2 — resolvidos (o que o caminho ensinou)

O teste de verificação (21 ferramentas registradas, 1 falha) levou 1min37s,
dos quais **~41s foram só contorno** dos dois problemas abaixo. Ambos estão
implementados e verificados ao vivo.

### T2 — `/work` gravável sem tocar no projeto

Antes: `-v pasta:/work:ro,Z`. `servidor.py` gravava `tarefas.json.tmp` e
morria em `OSError [Errno 30] Read-only file system`; o orquestrador gastou
5 chamadas de `ui_exec` copiando tudo para `/tmp/app` dentro do container.

Agora: `Mount::Overlay` (`-v pasta:/work:O`) é o padrão — o app escreve à
vontade e o que ele escreve morre com o container. `writable: true` continua
existindo para o caso raro em que o resultado precisa FICAR na pasta real
(`:rw,Z`), e `Mount::ReadOnly` (`:ro,Z`) ficou disponível para quem quiser a
trava antiga.

Duas coisas que só apareceram testando, e que o código agora documenta:

- **`:O` não pode vir com `:Z`** — o podman recusa com *"can only specify 1
  'z', 'Z', or 'O' option"*. O overlay já entra rotulado para o SELinux.
- **com `:O`, `podman inspect .Mounts` devolve o merge dir do storage**, não
  a pasta original. Por isso a pasta e o modo viraram *labels* do container
  (`orch.workdir`, `orch.mount`): é o que permite saber, depois, sobre qual
  pasta aquela sandbox está montada.

### T1 — a sandbox nasce de qualquer tool que não dependa de página

`ui_exec` sobe a sandbox sozinha quando ela não existe (política na função
`abre_sozinha`), porque a ordem natural de um teste é subir o serviço e só
então abrir a página. As tools que agem por referência (`ui_click e3`) NÃO
criam nada: numa sandbox recém-nascida não há referência alguma, e ali a
resposta certa continua sendo mandar chamar `ui_open`.

De lambuja, `container::attach` **readota** uma sandbox que já está de pé —
o processo do MCP reinicia muito mais que o container, e antes disso a única
saída era `ui_stop` e esperar o Chromium subir de novo. A readoção recusa
quando a pasta montada não é a pedida (seguir em frente daria um teste que
parece certo e mente) e quando se pede escrita real sobre uma sandbox
descartável viva (o modo não muda com o container vivo).

Toda resposta que cria ou readota uma sandbox começa dizendo qual pasta está
em `/work` e em que modo — sem isso o orquestrador não sabia onde mexeu.

### O que a revisão adversarial pegou depois

Submeti a mudança a quatro lentes (corretude, isolamento, contrato com o
modelo, testes) com dois refutadores por achado. Sobreviveram 11, todos
corrigidos. Os que valem lembrar:

- **`ui_exec` sem prazo congelava o MCP inteiro.** O servidor atende uma
  requisição por vez; um comando que não termina (subir servidor em primeiro
  plano é o jeito mais fácil de cair nisso) prendia tudo — inclusive o
  `ui_stop`, que era a única saída. Agora o corte é de 120 s e acontece DENTRO
  do container (`timeout` do coreutils), porque matar o `podman exec` daqui
  deixaria o processo vivo lá. A descrição da tool passou a dizer que o
  comando roda em primeiro plano e a mostrar o jeito certo de subir serviço.
- **Readotar chutava o que não sabia.** Sem os labels, `attach` afirmava a
  pasta e o modo que foram *pedidos* — as guardas ficavam tautológicas.
  Agora um label-marca (`orch.sandbox`) separa "sem pasta montada" de "não
  sei o que é isto", e o segundo caso recusa em vez de adivinhar.
- **Escrita real só era anunciada uma vez.** Uma sandbox aberta com
  `writable: true` mantinha a pasta do usuário montada por muitos turnos sem
  nenhuma linha lembrando disso. Agora TODA resposta dela começa com o aviso.
- **Sem teto, cada nome diferente virava um container** (navegador inteiro,
  512 MB de shm). Teto de 3, recusando com a lista das abertas.
- **`ui_stop` com nome errado dizia "já estava encerrada"** — ele marcaria a
  limpeza como feita enquanto a sandbox real seguia de pé. Agora responde
  "não há sandbox X" e lista as abertas, com pasta e modo.
- **O nome da sandbox ia cru no caminho do PNG**: um `..` gravaria bytes fora
  da pasta de screenshots. Passa por `sanitize`, como o nome do container.

Uma afirmação da revisão não se sustentou e ficou registrada no teste: acento
NÃO colapsa nomes (cada caractere fora do ASCII vira um `-` próprio), então
"verificação" e "verificacao" são sandboxes distintas; o que colapsa é
espaço/pontuação — e é por isso que o mapa de sessões passou a ser chaveado
pelo nome do container.

## T3 e T4 — resolvidos

**T3 — fonte de emoji.** `fonts-noto-color-emoji` entrou na imagem (11 MB numa
imagem que já tinha 884). A verificação foi visual, que é a única que vale
aqui: abri na sandbox uma página com `✅ ❌ ⚠️ 🔥 😀 🚀 📊` mais texto
acentuado, tirei o screenshot e li o PNG — todos coloridos, nenhum quadrado,
acentos corretos. Sem isso o orquestrador gastava passos decidindo se era
dado corrompido ou fonte faltando.

**T4 — sessão de processos.** Chegou aqui em duas voltas, e a segunda só
existe porque a revisão adversarial desconfiou da primeira e as medições
deram razão a ela.

*O que é verdade, medido.* O filho nasce líder de sessão com terminal de
controle (o `portable-pty` chama `setsid` + `TIOCSCTTY`). Por isso, quando ele
morre, o **kernel** já manda SIGHUP ao grupo em primeiro plano — e um neto
comum (`sleep &`) morria até com o código antigo (medido: 3 vivos → 0). Meu
primeiro teste usava exatamente esse caso e **passaria sem a mudança**. O que
vazava de verdade eram dois casos, os dois medidos com o código antigo:

- o neto que **ignora SIGHUP** — um servidor subido com `nohup`;
- o **job em grupo próprio** — com controle de jobs (`set -m`, o normal num
  shell interativo) cada job em segundo plano ganha grupo, e mirar o grupo do
  filho não o alcança.

*O desenho final.* O `kill` lê em `/proc`, **por número de sessão** (nunca
por nome), todo processo vivo da sessão da CLI, e sinaliza cada grupo
distinto: SIGHUP, espera que termina assim que esvazia (teto de 120 ms), e
SIGKILL só no grupo que ainda tiver processo vivo **reconferido naquele
instante**. Isso aposentou o `tcgetpgrp`, cujo número podia ser obsoleto.

*As guardas.* Este é o código mais perigoso do repositório — foi matando
processo por padrão de nome que eu já derrubei a TUI, a sessão que me
hospedava e o Claude Desktop do usuário.

1. `grupo_sinalizavel` recusa `0` ("o MEU grupo" na chamada do sistema),
   `-1` ("todos"), `1` (init) e o nosso grupo; e a sessão da CLI nunca pode
   ser a nossa.
2. **PID reciclado.** A thread que colhe o filho é a única que sabe que ele
   morreu, e agora ela registra isso. Depois da colheita, o `ChildKiller` do
   `portable-pty` — que é um `kill(pid, SIGHUP)` cru, sem conferência
   nenhuma — não é mais usado. E no instante da colheita ela tira uma foto:
   se a sessão já estava vazia, o número ficou livre e nada mais é procurado
   por ele. Se ainda havia neto vivo, o kernel mantém o número reservado
   enquanto ele existir.
3. O `kill` é idempotente: o `Drop` roda de novo depois de um fechamento
   explícito, e a segunda passada disparava sinal num número recém-liberado.

*O que a revisão ainda pegou.* A CLI que sai deixando um neto imune a SIGHUP
segurando o terminal **nunca chegava ao estado "encerrada"** — o card seguia
vivo, porque o estado dependia do EOF do PTY. Agora quem marca é a thread que
colhe o processo (medido: antes ficava falso; agora vira em milissegundos). E
o comentário que dizia que o `ChildKiller` encerrava "sem chance de escapar"
era falso: ele manda SIGHUP, não SIGKILL.

*Limite medido e fixado em teste:* neto que chama `setsid` abre sessão nova e
sobrevive. Alcançá-lo exigiria varrer processos por nome, que é justamente o
que este projeto não faz. O teste
`a_grandchild_that_calls_setsid_escapes_this_and_it_is_known` falha se algum
dia isso mudar.

*Buraco que sobrou, por desenho:* se a CLI sai sozinha e o card fica aberto,
o que ela deixou rodando segue vivo até o card fechar (ou a TUI sair). Matar
a sessão no instante da saída resolveria, mas mataria também um serviço que o
orquestrador talvez ainda queira testar — decisão do dono.

### Achados da revisão que ficaram para decidir

- **Rede da sandbox não é isolada** (medido): tem saída para a internet e
  **lê serviço do host que escute em `0.0.0.0`** — um servidor de teste com
  `index.html` foi lido de dentro da sandbox por `host.containers.internal`.
  Só o que escuta em `127.0.0.1` fica fora de alcance. Os comentários que
  diziam "sem rede do host" foram corrigidos; mudar a política (por exemplo
  bloquear o gateway do host mantendo a internet) é decisão pendente.
- **AppImage não roda na sandbox** como a descrição promete: não há
  `/dev/fuse` nem `fusermount` (medido). `APPIMAGE_EXTRACT_AND_RUN=1` na
  imagem contornaria sem FUSE — não testado por falta de um AppImage de prova.
- **Sem fonte CJK/devanágari** (medido: 0 fontes para 中, あ, अ). O caso
  relatado (emoji) está fechado; a classe de bug não, se algum projeto usar
  essas escritas.

## Nada em observação agora

Os quatro travamentos anotados (T1–T4) estão implementados e verificados. O
que sobrou não é travamento, e sim pedido de recurso — a seção "Próximos
passos pedidos", abaixo.

## O que a verificação provou

- O orquestrador **testa de verdade**: 21 ferramentas registradas, com
  `ui_type`/`ui_click` na interface e conferência de código HTTP a cada passo.
- O relatório dele bate com os fatos: a pasta real ficou intacta (sem
  `tarefas.json` no projeto), o screenshot mostra a tarefa concluída com
  acentos corretos e o emoji quadrado que ele descreveu.
- E ele é honesto sobre o que não sabe: no ciclo anterior reportou um `DELETE`
  404 que não conseguiu reproduzir, dizendo isso em vez de omitir ou inventar.

## Próximos passos pedidos (não implementados ainda)

Pedidos para completar a CLI — só o necessário, sem encher de opção inútil:

- **Trocar de provedor pela CLI**: hoje só por `Ctrl+p`/`/modelo` dentro da
  TUI. Falta um caminho de linha de comando (`orchestrator provider ...`) e
  persistência do escolhido.
- **Adicionar/remover CLIs de agente**: `Config.agent_clis` só se edita à mão
  no JSON. Falta comando para cadastrar uma CLI nova (nome, binário, args).
- **Editar o settings do Orchestrator manualmente**, para configurar coisas
  como um roteador de modelos (ex.: omnirouter) — hoje não há caminho pela
  interface, e o arquivo não é descoberto facilmente.

### Pedido para depois: layout de bancada (referência: Nami)

Ideia trazida pelo usuário a partir de um print do Nami ("AI Agent
Workbench"):

- **Barra lateral esquerda com abas**: `Sessions` (sessões logadas em cada
  provedor — ChatGPT/Codex, Gemini, Claude, Kimi Code, Grok, Groq…, com a
  opção de spawnar uma CLI específica a partir dali), `Workspace` (árvore de
  pastas do projeto e worktrees) e, no nosso caso, **uma aba a mais: a do
  Orchestrator**.
- **Direita**: os workflows.
- **Centro**: a grade de CLIs, cada card com cabeçalho (nome, comando, pasta,
  estado "live", controles de minimizar/maximizar/fechar).
- Trocar **sessão, pasta e workspace** tudo por dentro do app, sem sair e
  reabrir manualmente.
- **Distribuição**: AppImage para todas as distros Linux e build Windows via
  MinGW.

Nota que já vale anotar: AppImage do nosso próprio app é independente do
achado acima sobre AppImage *dentro da sandbox* (lá o problema é rodar
AppImage de terceiros sem FUSE; aqui é empacotar o Orchestrator).

## Memória semântica (feito — 2026-09-12)

Pedido: toda IA do Orchestrator (o chat e as CLIs) seguir as regras do dono
sem exceção, consultar a memória antes de desenvolver, registrar o que
aprende, pesquisar o que não sabe — com ChromaDB, reranker e uma instrução
invisível em todo prompt; memórias próprias das IAs no projeto; memórias
gerais do dono valendo em todo projeto; e o atalho Ctrl+Shift+W.

**O que havia de errado antes**: a busca semântica nunca funcionou. Só o MCP
e a CLI embedavam em 384d; a TUI e o hook gravavam 64d na MESMA tabela, e o
cosseno entre dimensões diferentes dá zero. As CLIs em PTY não recebiam
contexto nenhum, e não havia escopo global nem origem (dono × IA).

**Como ficou**
- **SQLite é a fonte da verdade**; o ChromaDB é um índice reconstruível
  (`orchestrator memory reindex`). `memories` ganhou escopo (projeto/global),
  origem (dono/IA), autor e o tipo `practice`. A migração recria a tabela sem
  perder nada.
- **`orchestrator-memoryd`** é o único processo que carrega modelo:
  `multilingual-e5-small` (vetores) + `jina-reranker-v2-base-multilingual`
  (cross-encoder). Socket unix, sai sozinho após 30 min ocioso. Busca
  HÍBRIDA: vizinhos do Chroma + termos exatos do SQLite → reranker → pesos
  de tipo, prioridade e origem (dono acima de IA).
- **Chroma em container podman** subido pelo próprio memoryd, fixado por
  digest, só no loopback. Falamos pela API HTTP v2, não pelo crate oficial:
  ele exige o `protoc` instalado para compilar, o que pesaria no AppImage e
  no MinGW.
- **Instrução invisível**: hook `UserPromptSubmit` injeta contrato +
  regras fixas do dono (prioridade ≥ 9) + índice do prompt. Vale para o chat
  e para as CLIs. Sem memoryd, cai no índice por palavras-chave e diz isso.
- **Trava por sessão**: o `PreToolUse` nega qualquer ferramenta que altere
  algo (inclusive `Bash`) até a sessão chamar `retrieve_memory`.
- **Painel Ctrl+Shift+W / F4** (abas Global · Projeto · IAs) e CLI
  `orchestrator memory add --global | --project`, `list`, `search`, `reindex`.

**Verificado ao vivo**
- Busca pelo SENTIDO, sem palavra em comum: "qual SGBD a gente usa?" →
  PostgreSQL; "como escrever a mensagem ao versionar?" → commits em
  português; "ferramenta de build do front?" → Vite (memória de IA).
  Projeto `outro` nunca vê a memória da `loja`.
- Latência com modelos carregados: ~170 ms por prompt. Modelos: 1,6 GB no
  primeiro download; carga de 6–7 s depois disso.
- **Corte de relevância calibrado nos números**: memórias relevantes tiveram
  relevância crua de 0,12 a 0,45; perguntas sem relação ("capital da
  França") não passaram de 0,06, mas os pesos punham uma regra de segurança
  p10 no topo. Corte em 0,08 na relevância crua — agora pergunta sem relação
  recebe "nada relevante".
- **Claude real** num projeto de rascunho: o contexto chegou pelo caminho
  semântico; a sequência foi `Bash` → **negado pela trava** →
  `retrieve_memory` → `Read` → `Write`, e o arquivo saiu com "PostgreSQL via
  sqlx". A consulta e o bloqueio ficaram registrados no banco.
- O memoryd subiu o `orchestrator-chroma` sozinho, e a porta não responde
  pelo IP da rede local.

**Defeitos que só apareceram testando**
- `cargo build --tests` não gera o binário do memoryd — o hook e a TUI o
  procuram ao lado deles; é preciso `cargo build`.
- Caminho de socket unix tem limite de 108 bytes; agora recusa com
  explicação.
- Um teste de integração gravou vetores de 4 dimensões na coleção de
  produção e o Chroma fixou a dimensão para sempre → testes usam coleção
  própria, e o nome da coleção de produção leva a dimensão.
- A busca por palavras-chave completava a lista com memória sem relação
  (score 0) → só entra o que tem relação.

- **Aberto (achado em 2026-09-12, não corrigido)**: o hook descobre o projeto
  pelo NOME da pasta atual (`project_of` em `crates/mcp-server/src/hook.rs`).
  Um `cd` para uma subpasta (`design/prototipo-app`) vira o projeto
  "prototipo-app": a sessão, que já tinha consultado, volta a ser barrada, e
  a consulta seguinte fica registrada com outro nome. Correção prevista:
  resolver pelo projeto cadastrado cujo diretório é o prefixo mais longo do
  cwd, e só então cair no nome da pasta. Quando não há `ORCHESTRATOR_PROJECT`
  (sessões fora da TUI), é o que vale.

**Decisões fora do plano original**
- Regras de segurança não entram todas em todo prompt: o hook já as impõe,
  o contrato diz quantas estão ativas e as relevantes aparecem no índice.
- O dono pode APAGAR nota de IA pelo painel, mas não editá-la.
- A aba Memórias do F2 ficou como espelho (mostra também as globais).

**Depende de você conferir**
- Ctrl+Shift+W no Konsole: a TUI liga o protocolo de teclado do kitty, que
  hoje faz o Konsole repassar os atalhos dele (bug 524571). Quando o Konsole
  for corrigido, a tecla pode voltar a fechar a aba — F4 funciona sempre.
- Sessões e TUIs abertas com os binários antigos quebram nas tools de
  memória (o banco já foi migrado): reabra. O hook novo é instalado sozinho
  na próxima vez que a TUI abrir cada projeto.

## App desktop + API de memória (em andamento — 2026-09-12)

Plano aprovado em fases: protótipo → núcleo compartilhado → modelos com
licença livre + API HTTP → página do globo → app Tauri → pacotes Linux.

**Fase 0 — protótipo visual: aprovado pelo dono.** Pedido junto: mais toques
intuitivos, para não virar "painel de controle de avião". As fontes que geram
o canvas estão em `design/prototipo-app/`.

**Fase 1 — núcleo compartilhado (`crates/engine`, em andamento)**
- `term`, `chat`, `agent_card`, `palette` e `picker_dir` saíram da TUI; a
  conversão de tecla do crossterm ficou na TUI (`keys.rs`).
- O estado e a lógica do App viraram `orchestrator_engine::Engine`. A TUI
  guarda só o que é de desenho (foco, tela, seleção, layout, mouse) e chega
  no núcleo por `Deref`.
- O núcleo não conhece foco: pede à interface por `EngineEvent`
  (`FocusGrid`, `FocusChat`, `ChooseModel`, `ShowHelp`).
- `Engine::run_command` executa os comandos `/`. A TUI e o app chamam o
  MESMO despacho.

**Fase 2 — troca de modelos (decidido por benchmark,
`crates/memoryd/examples/bench.rs`)**
- **O reranker atual (`jina-reranker-v2-base-multilingual`) é CC-BY-NC**:
  não pode ir num app publicado.
- Medido com 24 memórias (6 de verdade + 18 distrações) e 9 perguntas, 3 delas
  sem relação com nada:

| conjunto | acerta o 1º | relevante mais fraco × sem relação mais forte | 12 cand. | 24 cand. |
|---|---|---|---|---|
| jina (atual, NC) | 6/6 | 0,133 × 0,205 (não separa) | — | 640 ms |
| mmarco int8 (Apache) | 4/6 | 0,011 × 0,025 | 67 ms | 127 ms |
| mmarco fp32 (Apache) | 5/6 | 0,014 × 0,034 | 89 ms | 170 ms |
| **gte-multilingual int8 (Apache)** | **6/6** | **0,333 × 0,168** | 248 ms | 528 ms |

- **Escolha**: vetores `multilingual-e5-small` int8 (MIT; 6/6 no top-1 sozinho,
  ~5 ms por consulta) + reranker `gte-multilingual-reranker-base` int8.
  Busca enquanto digita = só vetor. Enter e contexto do prompt = reranker nas
  ~12 melhores.
- Lição: o corte de 0,08 do jina só funcionava com poucas memórias. Com
  distrações, pergunta sem relação chegou a 0,205. O corte será recalibrado
  com o gte (a separação medida sugere algo perto de 0,25).
- **Corte relativo (substituiu o corte único)**: pedido de tarefa ("vou criar a
  tabela de pedidos no banco") pôs a memória certa em 0,223 e distrações
  chegaram a 0,35 em outros prompts. Agora:
  - a MELHOR entra a partir de 0,20;
  - as demais precisam de 0,30 e de 80% da melhor.

  Pergunta sem relação continua voltando vazia.
- **Verificado ao vivo** (memoryd + curl, porta de teste):
  - download de 471 MB com checksum e progresso no health em ~24 s;
  - busca completa com reranker em ~136 ms; prévia só por vetor em ~8 ms;
  - contexto de tarefa agora traz o PostgreSQL;
  - grafo com 14 ligações de sentido;
  - proteções:
    - Host de fora → 421;
    - escrita sem token ou com token errado → 401;
    - CORS de origem estranha não liberado;
    - escuta só em 127.0.0.1 e pelo IP da rede não conecta;
  - token criado com permissão 0600.
- **A observar**: memória nova "pagamentos pelo gateway só no backend" não
  apareceu para "quem chama a API de cobrança?" (o reranker achou a relação
  fraca). E a prévia só por vetor não tem corte: E5 dá cosseno 0,80–0,88 para
  quase tudo; a página deixa claro que é prévia e que o Enter confirma.
- Checksums conferidos contra o Hugging Face:
  - E5 int8 `4d24e2bc…`;
  - gte int8 `ccf51dba…`;
  - mmarco quint8 `6c251376…`;
  - mmarco fp32 `3e9a03ed…`.

**Fase 3 — página da memória (feita, conferida por screenshot)**
- `web/` é um workspace npm:
  - `packages/tokens`: os tokens do protótipo;
  - `packages/globe`: o globo em 3d-force-graph + three, com reserva 2D sem WebGL e tooltips com texto escapado;
  - `apps/memory`: Vite + TypeScript, sem framework, fontes locais (@fontsource), nada carregado de fora.
- `npm run build:memory` (dentro de `web/`) gera `apps/memory/dist`. O memoryd embute a pasta no binário (rust-embed; em debug lê do disco) e serve `/` com CSP estrita (`script-src 'self'`, sem eval, `frame-ancestors 'none'`).
- O que tem:
  - busca ao digitar só por vetor e Enter com reranker;
  - filtros de projeto, tipo e autor;
  - resultados que acendem o globo e levam a câmera até o melhor;
  - detalhe da memória;
  - aba Lista e aba API com exemplos de `curl` usando a porta real;
  - "Editar memórias" pede o token (lembrado só na aba) e valida com um PUT num id que não existe (401 × 404);
  - apagar pede dois cliques;
  - links diretos `?q=…&projeto=…&vista=lista|api`.
- Conferido em 1440, 768 e 390 px (Chromium headless com SwiftShader). Três defeitos só apareceram nas imagens:
  - o globo nascia minúsculo → enquadra quando a simulação assenta;
  - voar até o nó durante o enquadramento deixava a tela vazia → o foco espera o enquadramento;
  - filtros cortados no celular → rótulos curtos em tela estreita.
- Não conferido ainda: criar, editar e apagar pela página (precisa colar o token) e a reserva 2D.
- Verificação visual: a extensão do Chrome estava desconectada e o Brave headless trava sem saída. O que funcionou: `flatpak run --filesystem=<pasta> org.chromium.Chromium --headless=new --use-angle=swiftshader --enable-unsafe-swiftshader --screenshot=…`.

**Fase 4 — app desktop (primeira versão feita)**
- `web/apps/desktop`: React 19 + cmdk + xterm.js 6. O crate Tauri fica em `web/apps/desktop/src-tauri`, membro do workspace Cargo.
- O app abre o MESMO `Engine` da TUI. Um laço em segundo plano faz o trabalho do laço da TUI e emite `estado` quando algo muda; os pedidos do núcleo saem como evento `pedido`.
- Comandos Tauri:
  - `estado`, `enviar` (passa por `Engine::run_command`) e `paleta`;
  - `trocar_workspace`, `focar_card` e `fechar_card`;
  - `resolver_decisao` e `iterar_agente`;
  - `assinar_terminal` (tela atual + bytes crus por `Channel`), `escrever_terminal` e `redimensionar_terminal`;
  - `memoria_api`.
- Telas:
  - workbench com projetos, workspaces, chat e grade de CLIs reais;
  - paleta com o estado de cada comando;
  - decisões, manual e painel de memória com o mesmo globo da página.
- Atalhos: a janela só usa Alt, F e Ctrl+Shift (Ctrl é das CLIs). Ctrl+K abre os comandos só fora do terminal.
- **Defeito real achado ao abrir a janela**: o WebKitGTK no Wayland daqui derruba a janela ("Error 71 dispatching to Wayland display"). O app agora define `WEBKIT_DISABLE_DMABUF_RENDERER=1` se ninguém definiu antes, e com isso a janela fica de pé.
- **Verificado**:
  - app aberto de verdade, isolado (config, banco, socket, porta e Chroma de teste): a janela fica aberta e a memória do app sobe e responde;
  - a interface foi conferida em Chromium headless com o IPC do Tauri simulado (`@tauri-apps/api/mocks`, `demo.html`): workbench com terminais coloridos, paleta, decisões e manual.
- A conferência visual mostrou e corrigiu:
  - abas "1 3" ilegíveis → "Workspace 1 · 3";
  - card sobrando pela metade → ocupa a linha;
  - ligadura da fonte trocando `|->` por seta;
  - placeholders cortados.
- **Incidente**: a primeira tentativa de ver o app capturou "a janela ativa" com o spectacle, e a janela ativa era outra, pessoal, do dono. A imagem foi apagada sem ser usada. Não se captura mais a tela do usuário.
- **Não verificado ainda**:
  - terminal de verdade no app: bytes do PTY chegando ao xterm pelo IPC real, e digitação voltando;
  - painel de memória dentro do app;
  - arrastar para redimensionar o chat, que ainda não existe no app (a largura é fixa).

**Fase 5 — pacotes Linux (gerados e testados)**
- `scripts/empacotar-linux.sh`:
  - gera a página da memória;
  - compila hook, MCP, memoryd e CLI em release;
  - junta os quatro como sidecars (nomes com o sufixo do alvo, passados por `--config` para não quebrar o build de desenvolvimento);
  - roda `tauri build`.
  - Saída em `target/release/bundle/`: `.deb` 37 MB, `.rpm` 37 MB, AppImage 139 MB.
- **Defeito do empacotamento**: o `strip` antigo embutido no linuxdeploy não reconhece a seção `.relr.dyn` das bibliotecas do Fedora 44 e aborta o AppImage. A saída foi `NO_STRIP=true`, já no script.
- **Teste de fumaça** (sem abrir janela):
  - o `.deb` e o `.rpm` trazem os 5 binários em `/usr/bin` (`orchestrator-desktop`, `orchestrator`, `orchestrator-hook`, `orchestrator-mcp`, `orchestrator-memoryd`);
  - o AppImage extraído tem os 5, e `orchestrator --version` responde;
  - o `.rpm` instala num Fedora 44 limpo (container), com zero bibliotecas faltando em todos os binários.
- **Limite de portabilidade medido**: compilados aqui, os binários exigem GLIBC_2.39, e o memoryd exige GLIBC_2.43. Só rodam em distros muito novas. Por isso o workflow `.github/workflows/desktop.yml` compila em Ubuntu 22.04 (glibc 2.35), e os pacotes para publicar devem sair de lá.
- **Windows (não feito)**: o workflow tem o job manual. Bloqueios reais:
  - memoryd, hook e MCP falam por socket unix → no Windows precisa de named pipe;
  - o encerramento de CLI mata a sessão POSIX (`libc::killpg`) → no Windows precisa de Job Object;
  - o ChromaDB sobe por podman.
- **Ainda não verificado**:
  - rodar o AppImage e o `.deb` fora do Fedora;
  - o terminal real e o painel de memória dentro do app;
  - arrastar para redimensionar o chat, que ainda não existe no app.

## Setup nativo (feito)

Abrir `orchestrator tui` numa pasta basta: se ela não é um projeto conhecido,
o Orchestrator lê de que se trata (manifesto, README, extensões), adota como
projeto, aponta a workspace 1 para ela, grava na configuração e se apresenta
no chat da própria workspace — sem trocar de conversa. Na segunda abertura não
duplica nem repete a apresentação; a conversa daquela workspace volta.
