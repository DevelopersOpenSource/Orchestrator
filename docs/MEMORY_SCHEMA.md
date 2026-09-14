# Schema da Memória

> **Nota:** este documento é do desenho inicial do projeto (fase 1–2) e não foi atualizado — várias partes já mudaram (TUI, app desktop, `/provedor`, o revisor de decisões). Veja o [README](../README.md) para o estado atual e o `TRAVAMENTOS.md` para decisões e armadilhas registradas ao longo do caminho.

Banco SQLite único (`~/.local/share/orchestrator/orchestrator.db`), acessado apenas pelo crate `memory` (rusqlite, WAL habilitado).

## Tabelas

### `memories`

```sql
CREATE TABLE memories (
    id          TEXT PRIMARY KEY,              -- UUID v4
    kind        TEXT NOT NULL CHECK (kind IN ('security','architecture','syntax','decision')),
    content     TEXT NOT NULL,                 -- texto já passado pelo scrubber de segredos
    tags        TEXT NOT NULL DEFAULT '[]',    -- JSON array de strings
    project     TEXT,                          -- escopo opcional (path/nome do projeto); NULL = global
    created_at  TEXT NOT NULL,                 -- RFC 3339 UTC
    updated_at  TEXT NOT NULL,
    scrubbed    INTEGER NOT NULL DEFAULT 0     -- 1 se o scrubber alterou o conteúdo
);
CREATE INDEX idx_memories_kind ON memories(kind);
CREATE INDEX idx_memories_project ON memories(project);
```

### `embeddings`

```sql
CREATE TABLE embeddings (
    memory_id  TEXT PRIMARY KEY REFERENCES memories(id) ON DELETE CASCADE,
    model      TEXT NOT NULL,        -- ex.: 'BAAI/bge-small-en-v1.5' (fastembed)
    dim        INTEGER NOT NULL,     -- ex.: 384
    vector     BLOB NOT NULL         -- f32 little-endian, dim * 4 bytes, L2-normalizado
);
```

### `decisions_log` (append-only)

```sql
CREATE TABLE decisions_log (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    session_id  TEXT,                -- session_id do Claude Code, se houver
    tool_name   TEXT,                -- ferramenta avaliada no PreToolUse
    input_json  TEXT,                -- input da ferramenta (scrubado)
    decision    TEXT NOT NULL CHECK (decision IN ('allow','deny','ask','approved','rejected')),
    decided_by  TEXT NOT NULL CHECK (decided_by IN ('rule','human','default')),
    rule_id     TEXT,                -- memories.id da regra que casou, se houver
    reason      TEXT,
    created_at  TEXT NOT NULL        -- RFC 3339 UTC
);
```

A camada de acesso expõe apenas `INSERT`/`SELECT` nesta tabela.

## Kinds

| Kind | Uso | Exemplo |
|---|---|---|
| `security` | Regras obrigatórias, aplicadas no `PreToolUse` | "Nunca rodar git push --force" |
| `architecture` | Decisões estruturais do projeto | "IPC entre crates é por socket Unix, não HTTP" |
| `decision` | Decisões pontuais já tomadas | "Escolhemos rusqlite em vez de sqlx" |
| `syntax` | Convenções de código/estilo | "Erros de domínio usam thiserror; anyhow só nas bordas" |

## Reranker

Score final de um candidato para a query `q`:

```
score = cosine(q, v) * weight(kind) + recency_boost + tag_boost
```

Pesos por `kind` (padrão, configuráveis):

| kind | peso |
|---|---|
| `security` | 1.5 |
| `architecture` | 1.2 |
| `decision` | 1.0 |
| `syntax` | 0.8 |

- `recency_boost`: até +0.05, decaimento exponencial com meia-vida de 30 dias sobre `updated_at`.
- `tag_boost`: +0.05 por tag da memória presente na query/contexto (máx. +0.1).

Busca: brute-force — carrega todos os vetores do `kind`/projeto filtrado, calcula cosseno, reranqueia, devolve `top_k` (padrão 8).

## Formato dos vetores

- `f32` little-endian concatenados (`dim * 4` bytes) no BLOB `vector`.
- Vetores são **L2-normalizados na escrita**, então cosseno = produto escalar.
- `model` e `dim` são gravados por linha; uma troca de modelo de embedding exige re-embed de tudo (migração detecta `model` divergente e reprocessa).
