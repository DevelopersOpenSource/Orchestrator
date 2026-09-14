# Modelo de Segurança

> **Nota:** este documento é do desenho inicial do projeto (fase 1–2) e não foi atualizado — várias partes já mudaram (TUI, app desktop, `/provedor`, o revisor de decisões). Veja o [README](../README.md) para o estado atual e o `TRAVAMENTOS.md` para decisões e armadilhas registradas ao longo do caminho.

## Modelo de ameaças

| Ameaça | Vetor | Mitigação |
|---|---|---|
| Agente executa operação destrutiva | Tool call (`rm -rf`, `git push --force`, DROP TABLE) | `PreToolUse` obrigatório com `permissionDecision: "deny"`/`"ask"`; ciclo de decisão crítica |
| Vazamento de segredos para a memória | Conteúdo com tokens/chaves embedado e persistido | Scrubber de segredos **antes** de embedar/gravar |
| Prompt injection via memória | Memória maliciosa injetada no contexto | Memórias `kind=security` só entram por ação humana; conteúdo recuperado é tratado como dado, não instrução |
| Escalada de privilégio do serviço | Serviço rodando como root/serviço de sistema | Menor privilégio: processo do usuário, `systemd --user`, sem capabilities extras |
| Adulteração do histórico de decisões | Edição/remoção de registros | Log de auditoria **append-only** (`decisions_log` sem UPDATE/DELETE na camada de acesso) |
| Dependências comprometidas | Supply chain de crates | `cargo audit` + `cargo deny` no CI; lockfile commitado |
| Binário adulterado na distribuição | Pacotes .deb/.rpm/MSI modificados | Binários e pacotes **assinados** (GPG para repos, Authenticode no Windows) |
| Exfiltração do banco de memória | Sync automático para nuvem | **Nunca** sincronizar o banco local automaticamente; export apenas manual e explícito |

## Regras

### Enforcement via PreToolUse (obrigatório)

Toda memória com `kind=security` é uma **regra**, não uma sugestão. O hook `PreToolUse` consulta o serviço a cada tool call; se alguma regra de segurança casar com a ferramenta/argumentos, a resposta é `deny` (ou `ask`, escalando para humano). Injeção no `SessionStart` é apenas informativa — o bloqueio real acontece no hook, que o agente não controla.

### Menor privilégio

- Serviço roda como o próprio usuário via `systemd --user` (ver `packaging/systemd/orchestrator-service.service`).
- Socket IPC com permissão `0600` no diretório runtime do usuário.
- Hardening no unit: `NoNewPrivileges=true`, `ProtectSystem=strict` com `ReadWritePaths` mínimos.

### Scrubber de segredos

Antes de qualquer conteúdo ser embedado ou persistido em `memories`, um scrubber remove/mascara padrões conhecidos: chaves de API (`sk-...`, `AKIA...`), tokens JWT, URLs com credenciais, blocos `PRIVATE KEY`, variáveis tipo `PASSWORD=`. Conteúdo scrubado é marcado; o original nunca é gravado.

### Auditoria append-only

`decisions_log` registra toda decisão (allow/deny/ask, quem decidiu, quando, contexto). A camada de acesso expõe apenas INSERT e SELECT. Rotação por arquivamento, nunca por reescrita.

### CI e distribuição

- `cargo audit` (RUSTSEC) e `cargo deny` (licenças, fontes, advisories) rodam em todo PR.
- Releases: binários assinados; checksums publicados; pacotes .deb/.rpm assinados com GPG, MSI com Authenticode.

### Dados locais

O banco SQLite fica em `~/.local/share/orchestrator/`. Nenhum sync automático; embeddings são gerados localmente (fastembed) — nada de conteúdo de memória sai da máquina.

## Política de decisões críticas

Exigem aprovação humana explícita (pausa + notificação + registro no log):

1. **Operações destrutivas**: remoção recursiva de arquivos, `git push --force`, reset de branches, drop/truncate de tabelas, desinstalação de pacotes.
2. **Mudanças de arquitetura**: alterações em memórias `kind=architecture`, mudanças de schema, adição de dependências novas ao workspace.
3. **Credenciais**: qualquer leitura/escrita de arquivos de segredos (`.env`, `~/.ssh`, keyrings), criação ou rotação de tokens.
4. **Rede externa**: chamadas a hosts fora de uma allowlist local, publicação de pacotes, criação de recursos em nuvem.

Tudo o mais segue a política padrão do agente, com log em `PostToolUse`.
