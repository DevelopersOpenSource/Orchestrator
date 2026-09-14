# Protocolo do Claude Code (stream-json)

> **Nota:** este documento é do desenho inicial do projeto (fase 1–2) e não foi atualizado — várias partes já mudaram (TUI, app desktop, `/provedor`, o revisor de decisões). Veja o [README](../README.md) para o estado atual e o `TRAVAMENTOS.md` para decisões e armadilhas registradas ao longo do caminho.

Como o `cli-adapter` fala com o Claude Code. Sempre subprocess + stdio; nunca automação de teclado.

## Invocação

```sh
claude -p "<prompt>" \
  --output-format stream-json \
  --verbose \
  --resume <session_id>          # opcional: continuar sessão existente
```

Flags usadas:

| Flag | Função |
|---|---|
| `-p / --print` | Modo não-interativo: processa o prompt e sai |
| `--output-format stream-json` | stdout vira NDJSON (um evento JSON por linha) |
| `--verbose` | Requerido pelo stream-json no modo -p |
| `--resume <id>` | Continua a sessão com aquele `session_id` |
| `--permission-mode` / `--allowedTools` | Ajuste fino de permissões (opcional) |
| `--mcp-config <arquivo>` | Carrega servidores MCP adicionais |

## Eventos (stdout, NDJSON)

### `system` / subtype `init`

Primeiro evento; o serviço guarda o `session_id` daqui para `--resume` futuro.

```json
{"type":"system","subtype":"init","session_id":"abc-123","model":"...","tools":["Bash","Edit",...],"cwd":"/..."}
```

### `assistant` / `user`

Mensagens completas da conversa (blocos `text`, `tool_use`, `tool_result` no formato da API Anthropic):

```json
{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"..."}]},"session_id":"abc-123"}
```

### `stream_event`

Deltas incrementais (quando streaming parcial está habilitado, ex. `--include-partial-messages`); envolvem eventos brutos de streaming da API (`content_block_delta` etc.). O adapter os usa apenas para exibição ao vivo; o estado canônico vem das mensagens completas.

### `result`

Último evento; encerra o turno:

```json
{"type":"result","subtype":"success","is_error":false,"result":"<texto final>",
 "session_id":"abc-123","duration_ms":1234,"num_turns":3,"total_cost_usd":0.01}
```

`subtype` pode ser `success`, `error_max_turns`, `error_during_execution`. O adapter mapeia `is_error`/`subtype` para `Result` tipado.

## Hooks

Configurados em `.claude/settings.json`. Cada hook recebe **JSON no stdin** e responde **JSON no stdout** (exit 0). Exit code 2 também bloqueia (stderr vira feedback), mas o Orchestrator usa sempre a resposta JSON estruturada.

### Entrada comum (stdin)

```json
{"session_id":"abc-123","transcript_path":"...","cwd":"...","hook_event_name":"PreToolUse",
 "tool_name":"Bash","tool_input":{"command":"rm -rf /"}}
```

### `PreToolUse` — saída (stdout)

```json
{
  "hookSpecificOutput": {
    "hookEventName": "PreToolUse",
    "permissionDecision": "deny",
    "permissionDecisionReason": "Regra de segurança: operação destrutiva exige aprovação humana"
  }
}
```

`permissionDecision`: `"allow"` | `"deny"` | `"ask"`. O Orchestrator usa `deny` para regras `kind=security` e `ask` para o ciclo de decisão crítica.

### `SessionStart` — saída

Injeta contexto no início da sessão:

```json
{"hookSpecificOutput":{"hookEventName":"SessionStart","additionalContext":"<regras e memórias relevantes>"}}
```

### `PostToolUse` / `Stop`

Recebem o resultado da ferramenta / fim do turno no stdin; o hook do Orchestrator apenas repassa ao serviço para gravar em `decisions_log` e responde `{}` (sem bloquear). `Stop` aceita `{"decision":"block","reason":"..."}` para forçar continuação — usado com parcimônia.

### Registro (`.claude/settings.json`)

```json
{
  "hooks": {
    "SessionStart": [{"hooks":[{"type":"command","command":"orchestrator-hook session-start"}]}],
    "PreToolUse":   [{"matcher":"*","hooks":[{"type":"command","command":"orchestrator-hook pre-tool-use"}]}],
    "PostToolUse":  [{"matcher":"*","hooks":[{"type":"command","command":"orchestrator-hook post-tool-use"}]}],
    "Stop":         [{"hooks":[{"type":"command","command":"orchestrator-hook stop"}]}]
  }
}
```

## `.mcp.json`

Registra o servidor MCP do Orchestrator (stdio) na raiz do projeto:

```json
{
  "mcpServers": {
    "orchestrator-memory": {
      "type": "stdio",
      "command": "orchestrator-mcp",
      "args": [],
      "env": {}
    }
  }
}
```

Ferramentas expostas: `retrieve_memory`, `store_memory`, `rerank` (ver `docs/ARCHITECTURE.md`).
