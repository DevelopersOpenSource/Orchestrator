//! Persistência de memórias, embeddings e log de decisões em SQLite.

use crate::relevance::{rank_relevant, Relevant};
use crate::scrub::scrub;
use crate::{DecisionEntry, Memory, MemoryError, MemoryKind, Origin, Scope, GLOBAL_PROJECT};
use anyhow::{anyhow, Context, Result};
use rusqlite::{params, Connection, OptionalExtension};
use std::collections::HashSet;
use std::path::Path;
use std::str::FromStr;
use std::sync::Mutex;

/// Store de memória sobre SQLite — a fonte da verdade.
///
/// Não carrega modelo nenhum: vetores e reranker moram no
/// `orchestrator-memoryd` (ver [`crate::daemon`]). A busca daqui é a léxica,
/// a reserva de quando o memoryd não responde.
pub struct MemoryStore {
    conn: Mutex<Connection>,
}

/// Colunas de `memories`, na ordem que [`row_to_memory`] lê.
const MEMORY_COLUMNS: &str =
    "id, project, kind, scope, origin, author, title, body, priority, created_at, updated_at";

/// DDL de `memories` (também usada para recriar a tabela na migração).
fn memories_ddl(table: &str) -> String {
    format!(
        "CREATE TABLE IF NOT EXISTS {table} (
            id TEXT PRIMARY KEY,
            project TEXT NOT NULL,
            kind TEXT NOT NULL CHECK(kind IN ('security','architecture','practice','syntax','decision')),
            scope TEXT NOT NULL DEFAULT 'project' CHECK(scope IN ('project','global')),
            origin TEXT NOT NULL DEFAULT 'user' CHECK(origin IN ('user','agent')),
            author TEXT NOT NULL DEFAULT '',
            title TEXT NOT NULL,
            body TEXT NOT NULL,
            priority INTEGER NOT NULL DEFAULT 0,
            indexed_model TEXT,
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL
        );"
    )
}

/// Uma memória a gravar. Os construtores dizem QUEM escreve — é isso que
/// separa a regra do dono da nota de uma IA.
#[derive(Debug, Clone)]
pub struct NewMemory<'a> {
    pub project: &'a str,
    pub kind: MemoryKind,
    pub scope: Scope,
    pub origin: Origin,
    pub author: &'a str,
    pub title: &'a str,
    pub body: &'a str,
    pub priority: i64,
}

impl<'a> NewMemory<'a> {
    /// Regra ou nota do dono, valendo no projeto.
    pub fn user(
        project: &'a str,
        kind: MemoryKind,
        title: &'a str,
        body: &'a str,
        priority: i64,
    ) -> Self {
        Self {
            project,
            kind,
            scope: Scope::Project,
            origin: Origin::User,
            author: "",
            title,
            body,
            priority,
        }
    }

    /// Memória geral de desenvolvimento do dono: vale em todo projeto.
    pub fn global(kind: MemoryKind, title: &'a str, body: &'a str, priority: i64) -> Self {
        Self {
            project: GLOBAL_PROJECT,
            kind,
            scope: Scope::Global,
            origin: Origin::User,
            author: "",
            title,
            body,
            priority,
        }
    }

    /// Memória própria de uma IA: fica no projeto, com o nome dela.
    pub fn agent(
        project: &'a str,
        author: &'a str,
        kind: MemoryKind,
        title: &'a str,
        body: &'a str,
        priority: i64,
    ) -> Self {
        Self {
            project,
            kind,
            scope: Scope::Project,
            origin: Origin::Agent,
            author,
            title,
            body,
            priority,
        }
    }
}

fn row_to_memory(row: &rusqlite::Row<'_>) -> rusqlite::Result<Memory> {
    let kind_str: String = row.get("kind")?;
    let kind = MemoryKind::from_str(&kind_str).unwrap_or(MemoryKind::Decision);
    let scope: String = row.get("scope")?;
    let origin: String = row.get("origin")?;
    Ok(Memory {
        id: row.get("id")?,
        project: row.get("project")?,
        kind,
        scope: Scope::parse(&scope),
        origin: Origin::parse(&origin),
        author: row.get("author")?,
        title: row.get("title")?,
        body: row.get("body")?,
        priority: row.get("priority")?,
        created_at: row.get("created_at")?,
        updated_at: row.get("updated_at")?,
    })
}

impl MemoryStore {
    /// Abre (ou cria) o banco no caminho dado e aplica as migrações.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let conn = Connection::open(path.as_ref())
            .with_context(|| format!("abrindo banco em {}", path.as_ref().display()))?;
        Self::from_connection(conn)
    }

    /// Abre um banco em memória (útil para testes).
    pub fn open_in_memory() -> Result<Self> {
        Self::from_connection(Connection::open_in_memory()?)
    }

    fn from_connection(conn: Connection) -> Result<Self> {
        let store = Self {
            conn: Mutex::new(conn),
        };
        store.migrate()?;
        Ok(store)
    }

    fn migrate(&self) -> Result<()> {
        let conn = self.lock();
        conn.execute_batch(
            r#"
            PRAGMA foreign_keys = ON;

            CREATE TABLE IF NOT EXISTS decisions_log (
                id TEXT PRIMARY KEY,
                project TEXT NOT NULL,
                session_id TEXT NOT NULL,
                action TEXT NOT NULL,
                decision TEXT NOT NULL,
                reason TEXT NOT NULL,
                created_at TEXT NOT NULL
            );

            CREATE INDEX IF NOT EXISTS idx_decisions_project ON decisions_log(project);

            CREATE TABLE IF NOT EXISTS pending_decisions (
                id TEXT PRIMARY KEY,
                project TEXT NOT NULL,
                session_id TEXT NOT NULL,
                summary TEXT NOT NULL,
                status TEXT NOT NULL DEFAULT 'pending'
                    CHECK(status IN ('pending','approved','denied')),
                resolution TEXT,
                created_at TEXT NOT NULL,
                resolved_at TEXT
            );

            CREATE INDEX IF NOT EXISTS idx_pending_status ON pending_decisions(status);

            -- Fila de comandos do orquestrador para a TUI (IPC entre o
            -- processo MCP, filho do `claude`, e o processo da TUI): o MCP
            -- enfileira, a TUI executa (abre CLI em PTY, envia prompt, mata)
            -- e marca done/failed com o resultado.
            CREATE TABLE IF NOT EXISTS cli_commands (
                id TEXT PRIMARY KEY,
                project TEXT NOT NULL,
                cli_name TEXT NOT NULL,
                kind TEXT NOT NULL CHECK(kind IN ('start','send','stop','read','menu','choose','key')),
                payload TEXT NOT NULL,
                status TEXT NOT NULL DEFAULT 'pending'
                    CHECK(status IN ('pending','done','failed')),
                result TEXT,
                created_at TEXT NOT NULL,
                resolved_at TEXT
            );

            CREATE INDEX IF NOT EXISTS idx_cli_commands_status
                ON cli_commands(project, status);

            -- Estado publicado pela TUI das CLIs gerenciadas (o MCP lê em
            -- cli_status): situação + último snapshot da tela vt100.
            CREATE TABLE IF NOT EXISTS cli_state (
                project TEXT NOT NULL,
                cli_name TEXT NOT NULL,
                status TEXT NOT NULL,
                screen TEXT NOT NULL,
                updated_at TEXT NOT NULL,
                PRIMARY KEY (project, cli_name)
            );

            -- Rastro das ferramentas que o orquestrador executou por conta
            -- própria (sandbox, navegador). As tools de CLI já ficam em
            -- `cli_commands`; sem esta tabela, "eu testei" era palavra dele
            -- contra nada — não havia como conferir o que rodou de fato.
            CREATE TABLE IF NOT EXISTS tool_log (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                project TEXT NOT NULL,
                tool TEXT NOT NULL,
                args TEXT NOT NULL,
                result TEXT NOT NULL,
                ok INTEGER NOT NULL,
                created_at TEXT NOT NULL
            );

            CREATE INDEX IF NOT EXISTS idx_tool_log_project
                ON tool_log(project, id);

            -- KV genérico de estado da UI (sessão do chat, layout, pastas
            -- por workspace) — sobrevive entre execuções da TUI.
            CREATE TABLE IF NOT EXISTS ui_state (
                key TEXT PRIMARY KEY,
                value TEXT NOT NULL,
                updated_at TEXT NOT NULL
            );

            -- Transcript do chat do orquestrador, para restaurar a conversa
            -- ao reabrir a TUI.
            -- Transcript por PROJETO e WORKSPACE: cada workspace tem a sua
            -- conversa, senão trabalhar em duas frentes mistura os contextos.
            CREATE TABLE IF NOT EXISTS chat_transcript (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                project TEXT NOT NULL,
                workspace INTEGER NOT NULL DEFAULT 0,
                who TEXT NOT NULL,
                text TEXT NOT NULL,
                created_at TEXT NOT NULL
            );

            "#,
        )?;
        self.migrate_cli_commands_kinds(&conn)?;
        self.migrate_pending_decisions(&conn)?;
        self.migrate_chat_workspace(&conn)?;
        self.migrate_memories(&conn)?;
        Ok(())
    }

    /// `pending_decisions` ganhou quem pediu, quem decide (o dono ou, no modo
    /// autônomo, o orquestrador), o tipo (ação ou pergunta), as alternativas,
    /// o motivo de escalar e a resposta. Bancos antigos recebem as colunas; o
    /// que já existia continua com o dono.
    fn migrate_pending_decisions(&self, conn: &Connection) -> Result<()> {
        let colunas: Vec<String> = conn
            .prepare("PRAGMA table_info(pending_decisions)")?
            .query_map([], |row| row.get::<_, String>(1))?
            .filter_map(std::result::Result::ok)
            .collect();
        for (nome, definicao) in [
            ("reviewer", "TEXT NOT NULL DEFAULT 'owner'"),
            ("requester", "TEXT NOT NULL DEFAULT ''"),
            ("kind", "TEXT NOT NULL DEFAULT 'action'"),
            ("options", "TEXT NOT NULL DEFAULT '[]'"),
            ("multiple", "INTEGER NOT NULL DEFAULT 0"),
            ("note", "TEXT"),
            ("answer", "TEXT"),
        ] {
            if !colunas.iter().any(|c| c == nome) {
                conn.execute_batch(&format!(
                    "ALTER TABLE pending_decisions ADD COLUMN {nome} {definicao};"
                ))?;
            }
        }
        Ok(())
    }

    /// O transcript ganhou a coluna `workspace` depois de criado; bancos
    /// antigos recebem a coluna (tudo que já existia vira workspace 0).
    fn migrate_chat_workspace(&self, conn: &Connection) -> Result<()> {
        let tem: bool = conn
            .prepare("PRAGMA table_info(chat_transcript)")?
            .query_map([], |row| row.get::<_, String>(1))?
            .filter_map(std::result::Result::ok)
            .any(|c| c == "workspace");
        if !tem {
            conn.execute_batch(
                "ALTER TABLE chat_transcript ADD COLUMN workspace INTEGER NOT NULL DEFAULT 0;",
            )?;
        }
        // O índice vem DEPOIS da coluna existir — criá-lo no batch principal
        // quebraria a abertura de um banco antigo.
        conn.execute_batch(
            "CREATE INDEX IF NOT EXISTS idx_chat_transcript_project
                 ON chat_transcript(project, workspace, id);",
        )?;
        Ok(())
    }

    /// `memories` ganhou escopo (projeto/global), origem (dono/IA), autor, o
    /// tipo `practice` e a marca de indexação. SQLite não altera `CHECK` com
    /// ALTER TABLE, então num banco antigo a tabela é recriada — sem perder
    /// nada: o que já existia vira memória do DONO, no projeto, pendente de
    /// indexar. A tabela `embeddings` sai: vetor agora mora só no ChromaDB,
    /// gerado por um único processo.
    fn migrate_memories(&self, conn: &Connection) -> Result<()> {
        let sql: Option<String> = conn
            .query_row(
                "SELECT sql FROM sqlite_master WHERE type = 'table' AND name = 'memories'",
                [],
                |row| row.get(0),
            )
            .optional()?;
        match sql {
            None => conn.execute_batch(&memories_ddl("memories"))?,
            Some(sql) if !sql.contains("'practice'") => {
                conn.execute_batch(&format!(
                    "PRAGMA foreign_keys = OFF;
                     BEGIN;
                     {ddl}
                     INSERT INTO memories_v2 (id, project, kind, scope, origin, author, title,
                                              body, priority, indexed_model, created_at, updated_at)
                         SELECT id, project, kind, 'project', 'user', '', title,
                                body, priority, NULL, created_at, updated_at
                         FROM memories;
                     DROP TABLE memories;
                     ALTER TABLE memories_v2 RENAME TO memories;
                     COMMIT;
                     PRAGMA foreign_keys = ON;",
                    ddl = memories_ddl("memories_v2")
                ))?;
            }
            Some(_) => {}
        }
        // Índices e tabelas auxiliares só DEPOIS de `memories` estar no
        // formato novo — criá-los no batch base quebraria um banco novo (a
        // tabela ainda não existe ali) e o `DROP TABLE` da migração os apaga.
        conn.execute_batch(
            "DROP TABLE IF EXISTS embeddings;
             CREATE INDEX IF NOT EXISTS idx_memories_project ON memories(project);
             CREATE INDEX IF NOT EXISTS idx_memories_project_kind ON memories(project, kind);
             CREATE INDEX IF NOT EXISTS idx_memories_visible ON memories(project, scope);
             CREATE TABLE IF NOT EXISTS memory_consults (
                 session_id TEXT NOT NULL,
                 project TEXT NOT NULL,
                 at TEXT NOT NULL,
                 PRIMARY KEY (session_id, project)
             );",
        )?;
        Ok(())
    }

    /// A fila de comandos ganhou o tipo `read` depois de criada, e SQLite não
    /// altera `CHECK` com ALTER TABLE. Como a fila é efêmera (comandos só
    /// fazem sentido com a TUI viva para executá-los), recriamos a tabela
    /// quando o CHECK antigo ainda estiver lá.
    fn migrate_cli_commands_kinds(&self, conn: &Connection) -> Result<()> {
        let sql: Option<String> = conn
            .query_row(
                "SELECT sql FROM sqlite_master WHERE type = 'table' AND name = 'cli_commands'",
                [],
                |row| row.get(0),
            )
            .optional()?;
        let Some(sql) = sql else { return Ok(()) };
        if sql.contains("'choose'") {
            return Ok(());
        }
        conn.execute_batch(
            r#"
            DROP TABLE cli_commands;
            CREATE TABLE cli_commands (
                id TEXT PRIMARY KEY,
                project TEXT NOT NULL,
                cli_name TEXT NOT NULL,
                kind TEXT NOT NULL CHECK(kind IN ('start','send','stop','read','menu','choose','key')),
                payload TEXT NOT NULL,
                status TEXT NOT NULL DEFAULT 'pending'
                    CHECK(status IN ('pending','done','failed')),
                result TEXT,
                created_at TEXT NOT NULL,
                resolved_at TEXT
            );
            CREATE INDEX IF NOT EXISTS idx_cli_commands_status
                ON cli_commands(project, status);
            "#,
        )?;
        Ok(())
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.conn.lock().expect("mutex do banco envenenado")
    }

    fn now() -> String {
        chrono::Utc::now().to_rfc3339()
    }

    /// Grava uma memória (com scrub de segredos), pendente de indexar.
    ///
    /// IA não grava memória global: o que vale em todo projeto é só do dono.
    pub fn add(&self, new: NewMemory<'_>) -> Result<Memory> {
        if new.scope == Scope::Global && new.origin == Origin::Agent {
            return Err(anyhow!(
                "memória global é só do dono — uma IA grava no projeto dela"
            ));
        }
        let project = match new.scope {
            Scope::Global => GLOBAL_PROJECT.to_string(),
            Scope::Project => new.project.to_string(),
        };
        let now = Self::now();
        let memory = Memory {
            id: uuid::Uuid::new_v4().to_string(),
            project,
            kind: new.kind,
            scope: new.scope,
            origin: new.origin,
            author: scrub(new.author),
            title: scrub(new.title),
            body: scrub(new.body),
            priority: new.priority,
            created_at: now.clone(),
            updated_at: now,
        };
        let conn = self.lock();
        conn.execute(
            "INSERT INTO memories (id, project, kind, scope, origin, author, title, body,
                                   priority, indexed_model, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, NULL, ?10, ?11)",
            params![
                memory.id,
                memory.project,
                memory.kind.as_str(),
                memory.scope.as_str(),
                memory.origin.as_str(),
                memory.author,
                memory.title,
                memory.body,
                memory.priority,
                memory.created_at,
                memory.updated_at
            ],
        )?;
        Ok(memory)
    }

    /// Atalho para uma memória do dono no projeto.
    pub fn add_memory(
        &self,
        project: &str,
        kind: MemoryKind,
        title: &str,
        body: &str,
        priority: i64,
    ) -> Result<Memory> {
        self.add(NewMemory::user(project, kind, title, body, priority))
    }

    /// Edita uma memória e a marca para reindexar.
    pub fn update(
        &self,
        id: &str,
        kind: MemoryKind,
        title: &str,
        body: &str,
        priority: i64,
    ) -> Result<Memory> {
        {
            let conn = self.lock();
            let n = conn.execute(
                "UPDATE memories
                 SET kind = ?1, title = ?2, body = ?3, priority = ?4, updated_at = ?5,
                     indexed_model = NULL
                 WHERE id = ?6",
                params![kind.as_str(), scrub(title), scrub(body), priority, Self::now(), id],
            )?;
            if n == 0 {
                return Err(MemoryError::NotFound(id.to_string()).into());
            }
        }
        self.get(id)?
            .ok_or_else(|| MemoryError::NotFound(id.to_string()).into())
    }

    /// Busca LÉXICA sobre o que o projeto enxerga (dele + globais) — a reserva
    /// de quando o memoryd não responde. Só devolve o que tem relação com a
    /// consulta. `kind` filtra por tipo.
    pub fn search(
        &self,
        project: &str,
        query: &str,
        top_k: usize,
        kind: Option<MemoryKind>,
    ) -> Result<Vec<Relevant>> {
        let candidates = self.list_visible(project, kind)?;
        Ok(rank_relevant(query, candidates, top_k))
    }

    /// Memórias de exatamente um projeto (use [`GLOBAL_PROJECT`] para as
    /// globais), opcionalmente por tipo.
    pub fn list(&self, project: &str, kind: Option<MemoryKind>) -> Result<Vec<Memory>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(&format!(
            "SELECT {MEMORY_COLUMNS} FROM memories
             WHERE project = ?1 AND (?2 IS NULL OR kind = ?2)
             ORDER BY updated_at DESC"
        ))?;
        let kind_str = kind.map(|k| k.as_str().to_string());
        let rows = stmt.query_map(params![project, kind_str], row_to_memory)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// O que um projeto ENXERGA: as memórias dele e as globais do dono.
    pub fn list_visible(&self, project: &str, kind: Option<MemoryKind>) -> Result<Vec<Memory>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(&format!(
            "SELECT {MEMORY_COLUMNS} FROM memories
             WHERE (project = ?1 OR scope = 'global') AND (?2 IS NULL OR kind = ?2)
             ORDER BY updated_at DESC"
        ))?;
        let kind_str = kind.map(|k| k.as_str().to_string());
        let rows = stmt.query_map(params![project, kind_str], row_to_memory)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Todas as memórias de todos os projetos (a página da memória em
    /// "todos os projetos").
    pub fn list_all(&self, kind: Option<MemoryKind>) -> Result<Vec<Memory>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(&format!(
            "SELECT {MEMORY_COLUMNS} FROM memories
             WHERE (?1 IS NULL OR kind = ?1)
             ORDER BY updated_at DESC"
        ))?;
        let kind_str = kind.map(|k| k.as_str().to_string());
        let rows = stmt.query_map(params![kind_str], row_to_memory)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Busca uma memória pelo id.
    pub fn get(&self, id: &str) -> Result<Option<Memory>> {
        let conn = self.lock();
        let memory = conn
            .query_row(
                &format!("SELECT {MEMORY_COLUMNS} FROM memories WHERE id = ?1"),
                params![id],
                row_to_memory,
            )
            .optional()?;
        Ok(memory)
    }

    /// Memórias que ainda não estão no índice deste modelo (novas, editadas,
    /// ou indexadas por um modelo que mudou).
    pub fn pending_index(&self, model: &str) -> Result<Vec<Memory>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(&format!(
            "SELECT {MEMORY_COLUMNS} FROM memories
             WHERE indexed_model IS NULL OR indexed_model != ?1"
        ))?;
        let rows = stmt.query_map(params![model], row_to_memory)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Marca memórias como indexadas por `model`.
    pub fn mark_indexed(&self, ids: &[String], model: &str) -> Result<()> {
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        for id in ids {
            tx.execute(
                "UPDATE memories SET indexed_model = ?1 WHERE id = ?2",
                params![model, id],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Marca TODAS as memórias como pendentes — o começo de um reindex.
    pub fn clear_indexed(&self) -> Result<usize> {
        let conn = self.lock();
        Ok(conn.execute("UPDATE memories SET indexed_model = NULL", [])?)
    }

    /// Todos os ids — o que um reindex do zero precisa pôr de volta.
    pub fn all_ids(&self) -> Result<Vec<String>> {
        let conn = self.lock();
        let mut stmt = conn.prepare("SELECT id FROM memories")?;
        let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Dos `ids`, quais ainda existem — o índice pode ter sobra de memória
    /// apagada enquanto o memoryd estava fora, e o SQLite é quem manda.
    pub fn existing_ids(&self, ids: &[String]) -> Result<HashSet<String>> {
        let conn = self.lock();
        let mut stmt = conn.prepare("SELECT 1 FROM memories WHERE id = ?1")?;
        let mut vivos = HashSet::new();
        for id in ids {
            if stmt.exists(params![id])? {
                vivos.insert(id.clone());
            }
        }
        Ok(vivos)
    }

    /// Registra que esta sessão de IA consultou a memória do projeto.
    pub fn record_consult(&self, session_id: &str, project: &str) -> Result<()> {
        let conn = self.lock();
        conn.execute(
            "INSERT OR REPLACE INTO memory_consults (session_id, project, at) VALUES (?1, ?2, ?3)",
            params![session_id, project, Self::now()],
        )?;
        Ok(())
    }

    /// Esta sessão já consultou a memória do projeto?
    pub fn has_consulted(&self, session_id: &str, project: &str) -> Result<bool> {
        let conn = self.lock();
        let mut stmt =
            conn.prepare("SELECT 1 FROM memory_consults WHERE session_id = ?1 AND project = ?2")?;
        Ok(stmt.exists(params![session_id, project])?)
    }

    /// Remove uma memória (tirá-la do índice vetorial é trabalho do memoryd).
    pub fn delete(&self, id: &str) -> Result<()> {
        let conn = self.lock();
        let n = conn.execute("DELETE FROM memories WHERE id = ?1", params![id])?;
        if n == 0 {
            return Err(MemoryError::NotFound(id.to_string()).into());
        }
        Ok(())
    }

    /// Registra uma decisão no log de auditoria (append-only; textos passam
    /// por scrub antes de serem gravados).
    pub fn log_decision(
        &self,
        project: &str,
        session_id: &str,
        action: &str,
        decision: &str,
        reason: &str,
    ) -> Result<DecisionEntry> {
        let entry = DecisionEntry {
            id: uuid::Uuid::new_v4().to_string(),
            project: project.to_string(),
            session_id: session_id.to_string(),
            action: scrub(action),
            decision: scrub(decision),
            reason: scrub(reason),
            created_at: Self::now(),
        };
        let conn = self.lock();
        conn.execute(
            "INSERT INTO decisions_log (id, project, session_id, action, decision, reason, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                entry.id,
                entry.project,
                entry.session_id,
                entry.action,
                entry.decision,
                entry.reason,
                entry.created_at
            ],
        )?;
        Ok(entry)
    }

    /// Lista decisões de um projeto, mais recentes primeiro.
    pub fn list_decisions(&self, project: &str) -> Result<Vec<DecisionEntry>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT id, project, session_id, action, decision, reason, created_at
             FROM decisions_log WHERE project = ?1 ORDER BY created_at DESC",
        )?;
        let rows = stmt.query_map(params![project], |row| {
            Ok(DecisionEntry {
                id: row.get("id")?,
                project: row.get("project")?,
                session_id: row.get("session_id")?,
                action: row.get("action")?,
                decision: row.get("decision")?,
                reason: row.get("reason")?,
                created_at: row.get("created_at")?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }
    /// Enfileira uma decisão crítica aguardando o dono (status `pending`).
    pub fn enqueue_decision(
        &self,
        project: &str,
        session_id: &str,
        summary: &str,
    ) -> Result<PendingDecision> {
        self.enqueue_decision_for(project, session_id, summary, REVIEWER_OWNER, "")
    }

    /// Enfileira uma decisão de ação para `reviewer` (o dono, ou o
    /// orquestrador no modo autônomo), lembrando quem pediu.
    pub fn enqueue_decision_for(
        &self,
        project: &str,
        session_id: &str,
        summary: &str,
        reviewer: &str,
        requester: &str,
    ) -> Result<PendingDecision> {
        self.insert_decision(PendingDecision {
            id: uuid::Uuid::new_v4().to_string(),
            project: project.to_string(),
            session_id: session_id.to_string(),
            summary: scrub(summary),
            status: "pending".to_string(),
            resolution: None,
            created_at: Self::now(),
            resolved_at: None,
            reviewer: reviewer.to_string(),
            requester: requester.to_string(),
            kind: KIND_ACTION.to_string(),
            options: Vec::new(),
            multiple: false,
            note: None,
            answer: None,
        })
    }

    /// Uma pergunta ao dono, com alternativas (escolha única ou múltipla) ou
    /// resposta livre quando `options` vem vazio.
    pub fn ask_owner(
        &self,
        project: &str,
        session_id: &str,
        question: &str,
        options: &[String],
        multiple: bool,
        requester: &str,
    ) -> Result<PendingDecision> {
        self.insert_decision(PendingDecision {
            id: uuid::Uuid::new_v4().to_string(),
            project: project.to_string(),
            session_id: session_id.to_string(),
            summary: scrub(question),
            status: "pending".to_string(),
            resolution: None,
            created_at: Self::now(),
            resolved_at: None,
            reviewer: REVIEWER_OWNER.to_string(),
            requester: requester.to_string(),
            kind: KIND_QUESTION.to_string(),
            options: options.iter().map(|o| scrub(o)).collect(),
            multiple,
            note: None,
            answer: None,
        })
    }

    fn insert_decision(&self, entry: PendingDecision) -> Result<PendingDecision> {
        let conn = self.lock();
        conn.execute(
            "INSERT INTO pending_decisions
                 (id, project, session_id, summary, status, created_at,
                  reviewer, requester, kind, options, multiple)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                entry.id,
                entry.project,
                entry.session_id,
                entry.summary,
                entry.status,
                entry.created_at,
                entry.reviewer,
                entry.requester,
                entry.kind,
                serde_json::to_string(&entry.options).unwrap_or_else(|_| "[]".into()),
                entry.multiple as i64,
            ],
        )?;
        Ok(entry)
    }

    /// Lista decisões pendentes (todas as mais recentes primeiro; com
    /// `only_pending`, apenas as ainda não resolvidas).
    pub fn list_pending_decisions(&self, only_pending: bool) -> Result<Vec<PendingDecision>> {
        let conn = self.lock();
        let sql = if only_pending {
            "SELECT * FROM pending_decisions WHERE status = 'pending' ORDER BY created_at DESC"
        } else {
            "SELECT * FROM pending_decisions ORDER BY created_at DESC"
        };
        let mut stmt = conn.prepare(sql)?;
        let rows = stmt.query_map([], decision_from_row)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Uma decisão pelo id.
    pub fn get_decision(&self, id: &str) -> Result<Option<PendingDecision>> {
        let conn = self.lock();
        Ok(conn
            .query_row(
                "SELECT * FROM pending_decisions WHERE id = ?1",
                params![id],
                decision_from_row,
            )
            .optional()?)
    }

    /// O orquestrador passa ao dono uma decisão que era dele, com o motivo.
    pub fn escalate_decision(&self, id: &str, note: &str) -> Result<PendingDecision> {
        let n = self.lock().execute(
            "UPDATE pending_decisions SET reviewer = ?2, note = ?3
             WHERE id = ?1 AND status = 'pending' AND reviewer = ?4",
            params![id, REVIEWER_OWNER, scrub(note), REVIEWER_ORCHESTRATOR],
        )?;
        if n == 0 {
            return Err(MemoryError::NotFound(id.to_string()).into());
        }
        let d = self
            .get_decision(id)?
            .ok_or_else(|| MemoryError::NotFound(id.to_string()))?;
        self.log_decision(&d.project, &d.session_id, &d.summary, "escalated", note)?;
        Ok(d)
    }

    /// O dono responde a uma pergunta.
    pub fn answer_question(&self, id: &str, answer: &str) -> Result<PendingDecision> {
        let n = self.lock().execute(
            "UPDATE pending_decisions
             SET status = 'approved', answer = ?2, resolution = ?2, resolved_at = ?3
             WHERE id = ?1 AND status = 'pending' AND kind = ?4",
            params![id, scrub(answer), Self::now(), KIND_QUESTION],
        )?;
        if n == 0 {
            return Err(MemoryError::NotFound(id.to_string()).into());
        }
        let d = self
            .get_decision(id)?
            .ok_or_else(|| MemoryError::NotFound(id.to_string()))?;
        self.log_decision(&d.project, &d.session_id, &d.summary, "answered", answer)?;
        Ok(d)
    }

    /// A decisão mais recente com este resumo (após scrub) para o par
    /// (projeto, sessão).
    pub fn latest_decision(
        &self,
        project: &str,
        session_id: &str,
        summary: &str,
    ) -> Result<Option<PendingDecision>> {
        let summary = scrub(summary);
        let conn = self.lock();
        Ok(conn
            .query_row(
                "SELECT * FROM pending_decisions
                 WHERE project = ?1 AND session_id = ?2 AND summary = ?3
                 ORDER BY created_at DESC LIMIT 1",
                params![project, session_id, summary],
                decision_from_row,
            )
            .optional()?)
    }

    /// Resolve uma decisão pendente (`approved`/`denied`), registrando
    /// também no log de auditoria.
    pub fn resolve_decision(
        &self,
        id: &str,
        approved: bool,
        resolution: &str,
    ) -> Result<()> {
        let status = if approved { "approved" } else { "denied" };
        let (project, session_id, summary) = {
            let conn = self.lock();
            let n = conn.execute(
                "UPDATE pending_decisions
                 SET status = ?2, resolution = ?3, resolved_at = ?4
                 WHERE id = ?1 AND status = 'pending'",
                params![id, status, scrub(resolution), Self::now()],
            )?;
            if n == 0 {
                return Err(MemoryError::NotFound(id.to_string()).into());
            }
            conn.query_row(
                "SELECT project, session_id, summary FROM pending_decisions WHERE id = ?1",
                params![id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                },
            )?
        };
        self.log_decision(&project, &session_id, &summary, status, resolution)?;
        Ok(())
    }

    /// Status da decisão mais recente com este resumo (após scrub) para o par
    /// (projeto, sessão): `"pending"`, `"approved"`, `"denied"` ou `None`.
    ///
    /// O hook usa isto para honrar aprovações: uma ação que o usuário já
    /// aprovou não deve pausar de novo, e uma já negada continua bloqueada —
    /// sem re-enfileirar duplicatas na fila.
    pub fn latest_decision_status(
        &self,
        project: &str,
        session_id: &str,
        summary: &str,
    ) -> Result<Option<String>> {
        Ok(self
            .latest_decision(project, session_id, summary)?
            .map(|d| d.status))
    }

    // ------------------------------------------------------------------
    // Fila de comandos de CLI (orquestrador → TUI) + estado publicado.
    // ------------------------------------------------------------------

    /// Enfileira um comando de CLI para a TUI executar (`start`/`send`/`stop`).
    pub fn enqueue_cli_command(
        &self,
        project: &str,
        cli_name: &str,
        kind: &str,
        payload: &str,
    ) -> Result<CliCommand> {
        let entry = CliCommand {
            id: uuid::Uuid::new_v4().to_string(),
            project: project.to_string(),
            cli_name: cli_name.to_string(),
            kind: kind.to_string(),
            payload: payload.to_string(),
            status: "pending".to_string(),
            result: None,
            created_at: Self::now(),
            resolved_at: None,
        };
        let conn = self.lock();
        conn.execute(
            "INSERT INTO cli_commands
                 (id, project, cli_name, kind, payload, status, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                entry.id,
                entry.project,
                entry.cli_name,
                entry.kind,
                entry.payload,
                entry.status,
                entry.created_at
            ],
        )?;
        Ok(entry)
    }

    /// Comandos pendentes de um projeto, mais antigos primeiro (ordem FIFO).
    pub fn pending_cli_commands(&self, project: &str) -> Result<Vec<CliCommand>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT * FROM cli_commands
             WHERE project = ?1 AND status = 'pending'
             ORDER BY created_at ASC",
        )?;
        let rows = stmt.query_map(params![project], row_to_cli_command)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Marca um comando como `done`/`failed` com o resultado textual.
    pub fn finish_cli_command(&self, id: &str, ok: bool, result: &str) -> Result<()> {
        let status = if ok { "done" } else { "failed" };
        let conn = self.lock();
        let n = conn.execute(
            "UPDATE cli_commands SET status = ?2, result = ?3, resolved_at = ?4
             WHERE id = ?1 AND status = 'pending'",
            params![id, status, result, Self::now()],
        )?;
        if n == 0 {
            return Err(MemoryError::NotFound(id.to_string()).into());
        }
        Ok(())
    }

    /// Status atual de um comando: `(status, result)` ou `None` se não existe.
    pub fn cli_command_status(&self, id: &str) -> Result<Option<(String, Option<String>)>> {
        let conn = self.lock();
        let row = conn
            .query_row(
                "SELECT status, result FROM cli_commands WHERE id = ?1",
                params![id],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?)),
            )
            .optional()?;
        Ok(row)
    }

    /// Publica (upsert) o estado de uma CLI gerenciada (TUI → banco).
    pub fn upsert_cli_state(
        &self,
        project: &str,
        cli_name: &str,
        status: &str,
        screen: &str,
    ) -> Result<()> {
        let conn = self.lock();
        conn.execute(
            "INSERT INTO cli_state (project, cli_name, status, screen, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(project, cli_name) DO UPDATE SET
                 status = excluded.status,
                 screen = excluded.screen,
                 updated_at = excluded.updated_at",
            params![project, cli_name, status, scrub(screen), Self::now()],
        )?;
        Ok(())
    }

    /// Estado de uma CLI: `(status, screen, updated_at)`.
    pub fn get_cli_state(
        &self,
        project: &str,
        cli_name: &str,
    ) -> Result<Option<(String, String, String)>> {
        let conn = self.lock();
        let row = conn
            .query_row(
                "SELECT status, screen, updated_at FROM cli_state
                 WHERE project = ?1 AND cli_name = ?2",
                params![project, cli_name],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                },
            )
            .optional()?;
        Ok(row)
    }

    /// Todas as CLIs conhecidas de um projeto: `(nome, status, updated_at)`.
    pub fn list_cli_states(&self, project: &str) -> Result<Vec<(String, String, String)>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT cli_name, status, updated_at FROM cli_state
             WHERE project = ?1 ORDER BY cli_name",
        )?;
        let rows = stmt.query_map(params![project], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Esquece todas as CLIs de um projeto.
    ///
    /// Nenhuma CLI sobrevive ao processo da TUI (os PTYs morrem com ela), então
    /// ao abrir a TUI o estado anterior é história: deixá-lo no banco faria o
    /// orquestrador enxergar CLIs que não existem mais e mandar tarefa para o
    /// vazio.
    pub fn clear_cli_states(&self, project: &str) -> Result<usize> {
        let conn = self.lock();
        let n = conn.execute(
            "DELETE FROM cli_state WHERE project = ?1",
            params![project],
        )?;
        Ok(n)
    }

    /// Remove o estado publicado de uma CLI (quando o card fecha).
    pub fn remove_cli_state(&self, project: &str, cli_name: &str) -> Result<()> {
        let conn = self.lock();
        conn.execute(
            "DELETE FROM cli_state WHERE project = ?1 AND cli_name = ?2",
            params![project, cli_name],
        )?;
        Ok(())
    }

    // ------------------------------------------------------------------
    // Estado da UI (KV) + transcript do chat.
    // ------------------------------------------------------------------

    /// Registra uma ferramenta que o orquestrador executou.
    ///
    /// Guarda um resumo curto: o log existe para CONFERIR o que ele fez, não
    /// para reter a saída inteira (que já custou contexto uma vez).
    pub fn log_tool_call(
        &self,
        project: &str,
        tool: &str,
        args: &str,
        result: &str,
        ok: bool,
    ) -> Result<()> {
        const MAX: usize = 300;
        let corta = |t: &str| -> String {
            let limpo = scrub(&t.replace('\n', " "));
            limpo.chars().take(MAX).collect()
        };
        let conn = self.lock();
        conn.execute(
            "INSERT INTO tool_log (project, tool, args, result, ok, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                project,
                tool,
                corta(args),
                corta(result),
                i64::from(ok),
                Self::now()
            ],
        )?;
        Ok(())
    }

    /// Últimas ferramentas executadas, da mais recente para a mais antiga.
    pub fn recent_tool_calls(
        &self,
        project: &str,
        limit: usize,
    ) -> Result<Vec<(String, String, String, bool, String)>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT tool, args, result, ok, created_at FROM tool_log
             WHERE project = ?1 ORDER BY id DESC LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![project, limit as i64], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, i64>(3)? != 0,
                row.get::<_, String>(4)?,
            ))
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Lê uma chave do estado da UI.
    pub fn ui_get(&self, key: &str) -> Result<Option<String>> {
        let conn = self.lock();
        let v = conn
            .query_row(
                "SELECT value FROM ui_state WHERE key = ?1",
                params![key],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        Ok(v)
    }

    /// Grava (upsert) uma chave do estado da UI.
    pub fn ui_set(&self, key: &str, value: &str) -> Result<()> {
        let conn = self.lock();
        conn.execute(
            "INSERT INTO ui_state (key, value, updated_at) VALUES (?1, ?2, ?3)
             ON CONFLICT(key) DO UPDATE SET
                 value = excluded.value, updated_at = excluded.updated_at",
            params![key, value, Self::now()],
        )?;
        Ok(())
    }

    /// Remove uma chave do estado da UI.
    pub fn ui_delete(&self, key: &str) -> Result<()> {
        let conn = self.lock();
        conn.execute("DELETE FROM ui_state WHERE key = ?1", params![key])?;
        Ok(())
    }

    /// Acrescenta uma linha ao transcript da workspace.
    pub fn append_chat_line(
        &self,
        project: &str,
        workspace: usize,
        who: &str,
        text: &str,
    ) -> Result<()> {
        let conn = self.lock();
        conn.execute(
            "INSERT INTO chat_transcript (project, workspace, who, text, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![project, workspace as i64, who, scrub(text), Self::now()],
        )?;
        Ok(())
    }

    /// Últimas `limit` linhas do transcript daquela workspace, em ordem.
    pub fn recent_chat_lines(
        &self,
        project: &str,
        workspace: usize,
        limit: usize,
    ) -> Result<Vec<(String, String)>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT who, text FROM (
                 SELECT id, who, text FROM chat_transcript
                 WHERE project = ?1 AND workspace = ?2 ORDER BY id DESC LIMIT ?3
             ) ORDER BY id ASC",
        )?;
        let rows = stmt.query_map(
            params![project, workspace as i64, limit as i64],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Apaga o transcript daquela workspace (comando `/nova`).
    pub fn clear_chat_transcript(&self, project: &str, workspace: usize) -> Result<()> {
        let conn = self.lock();
        conn.execute(
            "DELETE FROM chat_transcript WHERE project = ?1 AND workspace = ?2",
            params![project, workspace as i64],
        )?;
        Ok(())
    }
}

fn row_to_cli_command(row: &rusqlite::Row<'_>) -> rusqlite::Result<CliCommand> {
    Ok(CliCommand {
        id: row.get("id")?,
        project: row.get("project")?,
        cli_name: row.get("cli_name")?,
        kind: row.get("kind")?,
        payload: row.get("payload")?,
        status: row.get("status")?,
        result: row.get("result")?,
        created_at: row.get("created_at")?,
        resolved_at: row.get("resolved_at")?,
    })
}

/// Um comando de CLI enfileirado pelo orquestrador para a TUI executar.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct CliCommand {
    pub id: String,
    pub project: String,
    pub cli_name: String,
    /// `start` (payload = JSON `{command, args, cwd}`), `send` (payload =
    /// prompt) ou `stop` (payload vazio).
    pub kind: String,
    pub payload: String,
    pub status: String,
    pub result: Option<String>,
    pub created_at: String,
    pub resolved_at: Option<String>,
}

/// Uma decisão crítica pendente aguardando o usuário na TUI.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PendingDecision {
    pub id: String,
    pub project: String,
    pub session_id: String,
    pub summary: String,
    pub status: String,
    pub resolution: Option<String>,
    pub created_at: String,
    pub resolved_at: Option<String>,
    /// Quem decide: [`REVIEWER_OWNER`] ou [`REVIEWER_ORCHESTRATOR`].
    pub reviewer: String,
    /// Quem pediu (o nome da CLI), quando se sabe.
    pub requester: String,
    /// [`KIND_ACTION`] (aprovar/negar) ou [`KIND_QUESTION`] (pergunta ao dono).
    pub kind: String,
    /// As alternativas de uma pergunta (vazio = resposta livre).
    pub options: Vec<String>,
    /// A pergunta aceita mais de uma alternativa.
    pub multiple: bool,
    /// Por que o orquestrador passou a decisão ao dono.
    pub note: Option<String>,
    /// A resposta do dono a uma pergunta.
    pub answer: Option<String>,
}

impl PendingDecision {
    /// Espera pelo dono (e não pelo orquestrador).
    pub fn is_for_owner(&self) -> bool {
        self.reviewer != REVIEWER_ORCHESTRATOR
    }

    pub fn is_question(&self) -> bool {
        self.kind == KIND_QUESTION
    }
}

/// Quem decide uma pendência: o dono.
pub const REVIEWER_OWNER: &str = "owner";
/// Quem decide uma pendência: o orquestrador (modo autônomo).
pub const REVIEWER_ORCHESTRATOR: &str = "orchestrator";
/// Pendência de ação: aprovar ou negar.
pub const KIND_ACTION: &str = "action";
/// Pendência de pergunta: o dono responde.
pub const KIND_QUESTION: &str = "question";

fn decision_from_row(row: &rusqlite::Row) -> rusqlite::Result<PendingDecision> {
    let options: String = row.get("options")?;
    Ok(PendingDecision {
        id: row.get("id")?,
        project: row.get("project")?,
        session_id: row.get("session_id")?,
        summary: row.get("summary")?,
        status: row.get("status")?,
        resolution: row.get("resolution")?,
        created_at: row.get("created_at")?,
        resolved_at: row.get("resolved_at")?,
        reviewer: row.get("reviewer")?,
        requester: row.get("requester")?,
        kind: row.get("kind")?,
        options: serde_json::from_str(&options).unwrap_or_default(),
        multiple: row.get::<_, i64>("multiple")? != 0,
        note: row.get("note")?,
        answer: row.get("answer")?,
    })
}

#[cfg(test)]
mod decision_review_tests {
    use super::*;

    #[test]
    fn old_database_keeps_its_decisions_with_the_owner() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("memory.db");
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(
                "CREATE TABLE pending_decisions (
                    id TEXT PRIMARY KEY, project TEXT NOT NULL, session_id TEXT NOT NULL,
                    summary TEXT NOT NULL,
                    status TEXT NOT NULL DEFAULT 'pending' CHECK(status IN ('pending','approved','denied')),
                    resolution TEXT, created_at TEXT NOT NULL, resolved_at TEXT);
                 INSERT INTO pending_decisions (id, project, session_id, summary, created_at)
                 VALUES ('d1', 'p', 's', '[regra] git push --force', '2026-09-01');",
            )
            .unwrap();
        }
        let s = MemoryStore::open(&path).unwrap();
        let antigas = s.list_pending_decisions(true).unwrap();
        assert_eq!(antigas.len(), 1);
        assert!(antigas[0].is_for_owner());
        assert!(!antigas[0].is_question());
        // Abrir de novo não tenta recriar as colunas.
        drop(s);
        MemoryStore::open(&path).unwrap();
    }

    #[test]
    fn orchestrator_decision_can_be_escalated_to_the_owner() {
        let s = MemoryStore::open_in_memory().unwrap();
        let d = s
            .enqueue_decision_for("p", "s", "[migrações] sqlx migrate run", REVIEWER_ORCHESTRATOR, "backend")
            .unwrap();
        let lida = s.get_decision(&d.id).unwrap().unwrap();
        assert!(!lida.is_for_owner());
        assert_eq!(lida.requester, "backend");
        let escalada = s.escalate_decision(&d.id, "apaga dados de produção, não foi pedido").unwrap();
        assert!(escalada.is_for_owner());
        assert_eq!(escalada.note.as_deref(), Some("apaga dados de produção, não foi pedido"));
        // Só escala o que é do orquestrador.
        assert!(s.escalate_decision(&d.id, "de novo").is_err());
        assert!(s.list_decisions("p").unwrap().iter().any(|e| e.decision == "escalated"));
    }

    #[test]
    fn owner_answers_a_multiple_choice_question() {
        let s = MemoryStore::open_in_memory().unwrap();
        let q = s
            .ask_owner("p", "chat", "Qual banco usar?", &["Postgres".into(), "SQLite".into()], true, "orquestrador")
            .unwrap();
        let lida = s.get_decision(&q.id).unwrap().unwrap();
        assert!(lida.is_question() && lida.is_for_owner() && lida.multiple);
        assert_eq!(lida.options, ["Postgres", "SQLite"]);
        let respondida = s.answer_question(&q.id, "Postgres, SQLite").unwrap();
        assert_eq!(respondida.status, "approved");
        assert_eq!(respondida.answer.as_deref(), Some("Postgres, SQLite"));
        assert!(s.list_pending_decisions(true).unwrap().is_empty());
        // Ação não se responde como pergunta.
        let acao = s.enqueue_decision("p", "s", "x").unwrap();
        assert!(s.answer_question(&acao.id, "sim").is_err());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> MemoryStore {
        MemoryStore::open_in_memory().unwrap()
    }

    #[test]
    fn opens_on_disk_with_tempfile() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("memory.db");
        let s = MemoryStore::open(&path).unwrap();
        s.add_memory("proj", MemoryKind::Syntax, "titulo", "corpo", 0)
            .unwrap();
        assert_eq!(s.list("proj", None).unwrap().len(), 1);
        assert!(path.exists());
    }

    #[test]
    fn add_and_get_scrubs_secrets() {
        let s = store();
        let m = s
            .add_memory(
                "proj",
                MemoryKind::Security,
                "chave da api",
                "a chave é sk-abc123DEF456ghi789JKL012 nao vaze",
                1,
            )
            .unwrap();
        assert!(!m.body.contains("sk-abc123"));
        assert!(m.body.contains("[REDACTED]"));
        let fetched = s.get(&m.id).unwrap().unwrap();
        assert!(fetched.body.contains("[REDACTED]"));
    }

    #[test]
    fn search_returns_relevant_first_and_respects_project() {
        let s = store();
        s.add_memory("proj", MemoryKind::Syntax, "rust sqlite", "usar rusqlite bundled para sqlite", 0)
            .unwrap();
        s.add_memory("proj", MemoryKind::Syntax, "receita", "banana smoothie com aveia", 0)
            .unwrap();
        s.add_memory("outro", MemoryKind::Syntax, "rust sqlite", "usar rusqlite bundled para sqlite", 0)
            .unwrap();

        let results = s.search("proj", "rusqlite sqlite bundled", 10, None).unwrap();
        // Só o que tem relação: a receita não casa nada e fica de fora.
        assert_eq!(results.len(), 1);
        assert!(results.iter().all(|r| r.memory.project == "proj"));
        assert_eq!(results[0].memory.title, "rust sqlite");

        let top1 = s.search("proj", "rusqlite sqlite bundled", 1, None).unwrap();
        assert_eq!(top1.len(), 1);
    }

    #[test]
    fn search_filters_by_kind() {
        let s = store();
        s.add_memory("proj", MemoryKind::Security, "nunca logar tokens", "tokens fora dos logs", 0)
            .unwrap();
        s.add_memory("proj", MemoryKind::Syntax, "nunca logar tokens", "tokens fora dos logs", 0)
            .unwrap();
        let results = s
            .search("proj", "tokens logs", 10, Some(MemoryKind::Security))
            .unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].memory.kind, MemoryKind::Security);
    }

    #[test]
    fn list_and_delete() {
        let s = store();
        let m = s
            .add_memory("proj", MemoryKind::Decision, "usar tokio", "runtime async", 0)
            .unwrap();
        assert_eq!(s.list("proj", None).unwrap().len(), 1);
        assert_eq!(s.list("proj", Some(MemoryKind::Syntax)).unwrap().len(), 0);
        s.delete(&m.id).unwrap();
        assert!(s.list("proj", None).unwrap().is_empty());
        assert!(s.get(&m.id).unwrap().is_none());
        assert!(s.delete(&m.id).is_err());
    }

    #[test]
    fn a_project_sees_its_memories_and_the_global_ones_only() {
        let s = store();
        s.add(NewMemory::user("loja", MemoryKind::Architecture, "postgres", "x", 0)).unwrap();
        s.add(NewMemory::global(MemoryKind::Practice, "testes antes", "x", 5)).unwrap();
        s.add(NewMemory::user("outro", MemoryKind::Architecture, "mongo", "x", 0)).unwrap();
        let titulos: HashSet<String> = s
            .list_visible("loja", None)
            .unwrap()
            .into_iter()
            .map(|m| m.title)
            .collect();
        assert_eq!(titulos, HashSet::from(["postgres".to_string(), "testes antes".to_string()]));
        // A global mora no projeto "*" e é do dono.
        let globais = s.list(GLOBAL_PROJECT, None).unwrap();
        assert_eq!(globais.len(), 1);
        assert_eq!(globais[0].scope, Scope::Global);
        assert_eq!(globais[0].origin, Origin::User);
        // Busca léxica também enxerga a global.
        let achou = s.search("loja", "testes", 5, None).unwrap();
        assert!(achou.iter().any(|r| r.memory.title == "testes antes"));
    }

    #[test]
    fn an_agent_cannot_write_a_global_memory() {
        let s = store();
        let mut nova = NewMemory::agent("loja", "frontend", MemoryKind::Practice, "t", "b", 0);
        nova.scope = Scope::Global;
        assert!(s.add(nova).is_err());
        let m = s
            .add(NewMemory::agent("loja", "frontend", MemoryKind::Decision, "usar vite", "b", 0))
            .unwrap();
        assert_eq!(m.origin, Origin::Agent);
        assert_eq!(m.author, "frontend");
        assert_eq!(m.origin_label(), "IA: frontend");
    }

    #[test]
    fn indexing_bookkeeping_tracks_new_edited_and_model_changes() {
        let s = store();
        let a = s.add_memory("p", MemoryKind::Syntax, "a", "x", 0).unwrap();
        let b = s.add_memory("p", MemoryKind::Syntax, "b", "x", 0).unwrap();
        assert_eq!(s.pending_index("e5").unwrap().len(), 2);
        s.mark_indexed(&[a.id.clone(), b.id.clone()], "e5").unwrap();
        assert!(s.pending_index("e5").unwrap().is_empty());
        // Editar volta a ficar pendente.
        let editada = s.update(&a.id, MemoryKind::Practice, "a2", "y", 3).unwrap();
        assert_eq!(editada.kind, MemoryKind::Practice);
        assert_eq!(editada.priority, 3);
        let pend: Vec<String> = s.pending_index("e5").unwrap().into_iter().map(|m| m.id).collect();
        assert_eq!(pend, vec![a.id.clone()]);
        // Trocar de modelo pede tudo de novo.
        assert_eq!(s.pending_index("outro-modelo").unwrap().len(), 2);
        // Apagada some do conjunto de vivos.
        s.delete(&b.id).unwrap();
        let vivos = s.existing_ids(&[a.id.clone(), b.id.clone()]).unwrap();
        assert!(vivos.contains(&a.id) && !vivos.contains(&b.id));
        assert!(s.update("nao-existe", MemoryKind::Syntax, "t", "b", 0).is_err());
    }

    #[test]
    fn consults_are_per_session_and_project() {
        let s = store();
        assert!(!s.has_consulted("sessao-1", "loja").unwrap());
        s.record_consult("sessao-1", "loja").unwrap();
        s.record_consult("sessao-1", "loja").unwrap();
        assert!(s.has_consulted("sessao-1", "loja").unwrap());
        assert!(!s.has_consulted("sessao-2", "loja").unwrap());
        assert!(!s.has_consulted("sessao-1", "outro").unwrap());
    }

    #[test]
    fn an_old_database_migrates_without_losing_memories() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("antigo.db");
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(
                "PRAGMA foreign_keys = ON;
                 CREATE TABLE memories (
                    id TEXT PRIMARY KEY, project TEXT NOT NULL,
                    kind TEXT NOT NULL CHECK(kind IN ('security','architecture','syntax','decision')),
                    title TEXT NOT NULL, body TEXT NOT NULL, priority INTEGER NOT NULL DEFAULT 0,
                    created_at TEXT NOT NULL, updated_at TEXT NOT NULL);
                 CREATE INDEX idx_memories_project ON memories(project);
                 CREATE TABLE embeddings (
                    memory_id TEXT PRIMARY KEY REFERENCES memories(id) ON DELETE CASCADE,
                    vector BLOB NOT NULL, dim INTEGER NOT NULL, model TEXT NOT NULL);
                 INSERT INTO memories VALUES ('m1','loja','security','nunca rm','deny-regex: rm',10,'t','t');
                 INSERT INTO embeddings VALUES ('m1', x'00', 64, 'hash');",
            )
            .unwrap();
        }
        let s = MemoryStore::open(&path).unwrap();
        let todas = s.list("loja", None).unwrap();
        assert_eq!(todas.len(), 1, "a memória antiga tem de sobreviver");
        let m = &todas[0];
        assert_eq!((m.scope, m.origin, m.priority), (Scope::Project, Origin::User, 10));
        assert_eq!(s.pending_index("e5").unwrap().len(), 1, "fica pendente de indexar");
        // O tipo novo passa a ser aceito, e a tabela de vetores antiga saiu.
        s.add_memory("loja", MemoryKind::Practice, "p", "b", 0).unwrap();
        {
            let conn = s.lock();
            let tem_embeddings: bool = conn
                .prepare("SELECT 1 FROM sqlite_master WHERE name = 'embeddings'")
                .unwrap()
                .exists([])
                .unwrap();
            assert!(!tem_embeddings);
        }
        // Reabrir não migra de novo nem duplica.
        drop(s);
        let s = MemoryStore::open(&path).unwrap();
        assert_eq!(s.list("loja", None).unwrap().len(), 2);
    }

    #[test]
    fn a_fresh_database_opens_with_the_new_schema() {
        let dir = tempfile::tempdir().unwrap();
        let s = MemoryStore::open(dir.path().join("novo.db")).unwrap();
        s.add(NewMemory::global(MemoryKind::Practice, "g", "b", 9)).unwrap();
        assert_eq!(s.list_visible("qualquer", None).unwrap().len(), 1);
    }

    #[test]
    fn pending_decision_lifecycle() {
        let s = store();
        let p = s
            .enqueue_decision("proj", "sess1", "trocar arquitetura do banco?")
            .unwrap();
        assert_eq!(s.list_pending_decisions(true).unwrap().len(), 1);
        s.resolve_decision(&p.id, true, "pode trocar").unwrap();
        assert!(s.list_pending_decisions(true).unwrap().is_empty());
        let all = s.list_pending_decisions(false).unwrap();
        assert_eq!(all[0].status, "approved");
        // resolver de novo falha; e a resolução foi auditada
        assert!(s.resolve_decision(&p.id, false, "x").is_err());
        assert_eq!(s.list_decisions("proj").unwrap().len(), 1);
    }

    #[test]
    fn latest_decision_status_tracks_resolution() {
        let s = store();
        let summary = "[migração] Bash: sqlx migrate run";
        assert!(s
            .latest_decision_status("p", "sess", summary)
            .unwrap()
            .is_none());
        let d = s.enqueue_decision("p", "sess", summary).unwrap();
        assert_eq!(
            s.latest_decision_status("p", "sess", summary)
                .unwrap()
                .as_deref(),
            Some("pending")
        );
        s.resolve_decision(&d.id, true, "pode migrar").unwrap();
        assert_eq!(
            s.latest_decision_status("p", "sess", summary)
                .unwrap()
                .as_deref(),
            Some("approved")
        );
        // sessão ou projeto diferentes não casam
        assert!(s
            .latest_decision_status("p", "outra", summary)
            .unwrap()
            .is_none());
        assert!(s
            .latest_decision_status("outro", "sess", summary)
            .unwrap()
            .is_none());
    }

    #[test]
    fn decision_log_appends_and_scrubs() {
        let s = store();
        s.log_decision("proj", "sess1", "deploy", "aprovado", "password=abc123 valido")
            .unwrap();
        s.log_decision("proj", "sess1", "rollback", "negado", "sem motivo")
            .unwrap();
        let entries = s.list_decisions("proj").unwrap();
        assert_eq!(entries.len(), 2);
        let deploy = entries.iter().find(|e| e.action == "deploy").unwrap();
        assert!(!deploy.reason.contains("abc123"));
        assert!(deploy.reason.contains("[REDACTED]"));
        assert!(s.list_decisions("outro").unwrap().is_empty());
    }

    #[test]
    fn cli_command_queue_roundtrip() {
        let s = store();
        let cmd = s
            .enqueue_cli_command("proj", "frontend", "start", "{}")
            .unwrap();
        assert_eq!(s.pending_cli_commands("proj").unwrap().len(), 1);
        assert_eq!(
            s.cli_command_status(&cmd.id).unwrap(),
            Some(("pending".to_string(), None))
        );
        s.finish_cli_command(&cmd.id, true, "ok").unwrap();
        assert!(s.pending_cli_commands("proj").unwrap().is_empty());
        assert_eq!(
            s.cli_command_status(&cmd.id).unwrap(),
            Some(("done".to_string(), Some("ok".to_string())))
        );
        // Finalizar de novo (já não está pending) falha.
        assert!(s.finish_cli_command(&cmd.id, true, "de novo").is_err());
    }

    #[test]
    fn cli_commands_are_fifo_and_scoped_by_project() {
        let s = store();
        s.enqueue_cli_command("p1", "a", "start", "{}").unwrap();
        s.enqueue_cli_command("p1", "b", "start", "{}").unwrap();
        s.enqueue_cli_command("p2", "c", "start", "{}").unwrap();
        let p1 = s.pending_cli_commands("p1").unwrap();
        assert_eq!(p1.len(), 2);
        assert_eq!(p1[0].cli_name, "a");
        assert_eq!(p1[1].cli_name, "b");
        assert_eq!(s.pending_cli_commands("p2").unwrap().len(), 1);
    }

    #[test]
    fn cli_state_upsert_and_list() {
        let s = store();
        s.upsert_cli_state("proj", "frontend", "working", "tela 1")
            .unwrap();
        s.upsert_cli_state("proj", "frontend", "idle", "tela 2")
            .unwrap();
        let (status, screen, _) = s.get_cli_state("proj", "frontend").unwrap().unwrap();
        assert_eq!(status, "idle");
        assert_eq!(screen, "tela 2");
        s.upsert_cli_state("proj", "backend", "working", "outra")
            .unwrap();
        let all = s.list_cli_states("proj").unwrap();
        assert_eq!(all.len(), 2);
        s.remove_cli_state("proj", "frontend").unwrap();
        assert!(s.get_cli_state("proj", "frontend").unwrap().is_none());
        assert_eq!(s.list_cli_states("proj").unwrap().len(), 1);
    }

    #[test]
    fn cli_commands_accepts_the_read_kind_after_migration() {
        // Banco antigo: tabela com o CHECK sem 'read'.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("velho.db");
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(
                "CREATE TABLE cli_commands (
                     id TEXT PRIMARY KEY, project TEXT NOT NULL, cli_name TEXT NOT NULL,
                     kind TEXT NOT NULL CHECK(kind IN ('start','send','stop')),
                     payload TEXT NOT NULL, status TEXT NOT NULL DEFAULT 'pending'
                         CHECK(status IN ('pending','done','failed')),
                     result TEXT, created_at TEXT NOT NULL, resolved_at TEXT);",
            )
            .unwrap();
        }
        let s = MemoryStore::open(&path).unwrap();
        // Depois da migração o tipo novo é aceito.
        let cmd = s
            .enqueue_cli_command("p", "frontend", "read", "{}")
            .unwrap();
        assert_eq!(s.pending_cli_commands("p").unwrap().len(), 1);
        s.finish_cli_command(&cmd.id, true, "ok").unwrap();
        // E a migração não repete na próxima abertura.
        drop(s);
        let s = MemoryStore::open(&path).unwrap();
        assert!(s.enqueue_cli_command("p", "x", "read", "{}").is_ok());
    }

    #[test]
    fn each_workspace_keeps_its_own_conversation() {
        let s = store();
        s.append_chat_line("p", 0, "você", "assunto da workspace 1").unwrap();
        s.append_chat_line("p", 2, "você", "assunto da workspace 3").unwrap();
        let ws0 = s.recent_chat_lines("p", 0, 10).unwrap();
        let ws2 = s.recent_chat_lines("p", 2, 10).unwrap();
        assert_eq!(ws0.len(), 1);
        assert_eq!(ws2.len(), 1);
        assert!(ws0[0].1.contains("workspace 1"));
        assert!(ws2[0].1.contains("workspace 3"));
        // Zerar uma não afeta a outra.
        s.clear_chat_transcript("p", 0).unwrap();
        assert!(s.recent_chat_lines("p", 0, 10).unwrap().is_empty());
        assert_eq!(s.recent_chat_lines("p", 2, 10).unwrap().len(), 1);
    }

    #[test]
    fn old_transcripts_migrate_into_workspace_zero() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("antigo.db");
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(
                "CREATE TABLE chat_transcript (
                     id INTEGER PRIMARY KEY AUTOINCREMENT, project TEXT NOT NULL,
                     who TEXT NOT NULL, text TEXT NOT NULL, created_at TEXT NOT NULL);
                 INSERT INTO chat_transcript (project, who, text, created_at)
                 VALUES ('p','você','conversa antiga','2026-01-01');",
            )
            .unwrap();
        }
        let s = MemoryStore::open(&path).unwrap();
        let linhas = s.recent_chat_lines("p", 0, 10).unwrap();
        assert_eq!(linhas.len(), 1, "a conversa antiga não pode sumir");
        assert_eq!(linhas[0].1, "conversa antiga");
    }

    #[test]
    fn clearing_cli_states_only_touches_the_given_project() {
        let s = store();
        s.upsert_cli_state("p1", "a", "idle", "").unwrap();
        s.upsert_cli_state("p1", "b", "working", "").unwrap();
        s.upsert_cli_state("p2", "c", "idle", "").unwrap();
        assert_eq!(s.clear_cli_states("p1").unwrap(), 2);
        assert!(s.list_cli_states("p1").unwrap().is_empty());
        assert_eq!(s.list_cli_states("p2").unwrap().len(), 1);
    }

    #[test]
    fn tool_log_records_what_the_orchestrator_actually_ran() {
        let s = store();
        s.log_tool_call("p", "ui_open", r#"{"url":"/work/index.html"}"#, "2 elementos", true)
            .unwrap();
        s.log_tool_call("p", "ui_click", r#"{"ref":"e2"}"#, "falhou", false)
            .unwrap();
        s.log_tool_call("outro", "ui_open", "{}", "ok", true).unwrap();

        let calls = s.recent_tool_calls("p", 10).unwrap();
        assert_eq!(calls.len(), 2, "outro projeto não entra");
        // Mais recente primeiro.
        assert_eq!(calls[0].0, "ui_click");
        assert!(!calls[0].3, "falha é registrada como falha");
        assert_eq!(calls[1].0, "ui_open");
        assert!(calls[1].3);
    }

    #[test]
    fn tool_log_trims_and_scrubs() {
        let s = store();
        let gigante = "x".repeat(5000);
        s.log_tool_call("p", "ui_exec", &gigante, "senha=abc123secreta rodou", true)
            .unwrap();
        let c = s.recent_tool_calls("p", 1).unwrap();
        assert!(c[0].1.chars().count() <= 300, "argumento não foi cortado");
        assert!(!c[0].2.contains("abc123secreta"), "segredo vazou: {}", c[0].2);
        assert!(c[0].2.contains("[REDACTED]"));
    }

    #[test]
    fn ui_state_kv_roundtrip() {
        let s = store();
        assert!(s.ui_get("chat.session.proj").unwrap().is_none());
        s.ui_set("chat.session.proj", "sess-123").unwrap();
        assert_eq!(
            s.ui_get("chat.session.proj").unwrap().as_deref(),
            Some("sess-123")
        );
        s.ui_set("chat.session.proj", "sess-456").unwrap();
        assert_eq!(
            s.ui_get("chat.session.proj").unwrap().as_deref(),
            Some("sess-456")
        );
        s.ui_delete("chat.session.proj").unwrap();
        assert!(s.ui_get("chat.session.proj").unwrap().is_none());
    }

    #[test]
    fn chat_transcript_persists_and_restores_order() {
        let s = store();
        s.append_chat_line("proj", 0, "você", "oi").unwrap();
        s.append_chat_line("proj", 0, "orchestrator", "olá!").unwrap();
        s.append_chat_line("outro", 0, "você", "não deve aparecer")
            .unwrap();
        let lines = s.recent_chat_lines("proj", 0, 10).unwrap();
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0], ("você".to_string(), "oi".to_string()));
        assert_eq!(lines[1], ("orchestrator".to_string(), "olá!".to_string()));
        s.clear_chat_transcript("proj", 0).unwrap();
        assert!(s.recent_chat_lines("proj", 0, 10).unwrap().is_empty());
    }

    #[test]
    fn chat_transcript_limit_keeps_most_recent_in_order() {
        let s = store();
        for i in 0..5 {
            s.append_chat_line("proj", 0, "você", &format!("msg{i}")).unwrap();
        }
        let lines = s.recent_chat_lines("proj", 0, 3).unwrap();
        assert_eq!(
            lines.iter().map(|(_, t)| t.as_str()).collect::<Vec<_>>(),
            vec!["msg2", "msg3", "msg4"]
        );
    }
}
