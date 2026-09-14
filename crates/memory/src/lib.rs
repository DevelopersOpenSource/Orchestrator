//! # orchestrator-memory
//!
//! Registros de memória do Orchestrator e tudo que é leve o bastante para
//! rodar em qualquer processo (TUI, hook, MCP, CLI):
//!
//! - [`store::MemoryStore`]: SQLite — a FONTE DA VERDADE das memórias, do log
//!   de decisões e das filas;
//! - [`relevance`]: ranqueamento léxico, o caminho de reserva quando o serviço
//!   de memória não responde;
//! - [`rerank`]: pesos de tipo, prioridade e origem, comuns aos dois caminhos;
//! - [`contract`]: o texto invisível que acompanha cada prompt;
//! - [`daemon`]: protocolo e cliente do `orchestrator-memoryd`, o único
//!   processo que carrega modelos e fala com o ChromaDB;
//! - [`scrub`]: remoção de segredos antes de qualquer gravação.
//!
//! Embeddings e reranker NÃO moram aqui. Carregar modelos ONNX num hook que
//! roda a cada evento custaria segundos — e, quando cada processo embedava
//! com o próprio modelo, vetores de dimensões diferentes foram parar na mesma
//! tabela e a busca semântica devolvia similaridade zero sem ninguém notar.

pub mod contract;
pub mod daemon;
pub mod defaults;
pub mod embed;
pub mod relevance;
pub mod rerank;
pub mod scrub;
pub mod store;

use serde::{Deserialize, Serialize};

/// Erros do motor de memória.
#[derive(Debug, thiserror::Error)]
pub enum MemoryError {
    /// Erro vindo do SQLite.
    #[error("erro de banco de dados: {0}")]
    Database(#[from] rusqlite::Error),
    /// Erro na geração de embeddings.
    #[error("erro de embedding: {0}")]
    Embedding(String),
    /// Tipo de memória inválido.
    #[error("tipo de memória inválido: {0}")]
    InvalidKind(String),
    /// Memória não encontrada.
    #[error("memória não encontrada: {0}")]
    NotFound(String),
}

/// Projeto das memórias GLOBAIS do dono — as que valem em todo projeto.
pub const GLOBAL_PROJECT: &str = "*";

/// Tipos de memória suportados.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MemoryKind {
    Security,
    Architecture,
    /// Convenção ou preferência de desenvolvimento ("sempre escrever teste
    /// antes", "commits em português") — o tipo das regras gerais do dono.
    Practice,
    Syntax,
    Decision,
}

impl MemoryKind {
    /// Todos os tipos, na ordem em que aparecem para o dono.
    pub const ALL: [MemoryKind; 5] = [
        MemoryKind::Security,
        MemoryKind::Architecture,
        MemoryKind::Practice,
        MemoryKind::Syntax,
        MemoryKind::Decision,
    ];

    /// Nome canônico usado no banco.
    pub fn as_str(&self) -> &'static str {
        match self {
            MemoryKind::Security => "security",
            MemoryKind::Architecture => "architecture",
            MemoryKind::Practice => "practice",
            MemoryKind::Syntax => "syntax",
            MemoryKind::Decision => "decision",
        }
    }
}

impl std::str::FromStr for MemoryKind {
    type Err = MemoryError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "security" => Ok(MemoryKind::Security),
            "architecture" => Ok(MemoryKind::Architecture),
            "practice" => Ok(MemoryKind::Practice),
            "syntax" => Ok(MemoryKind::Syntax),
            "decision" => Ok(MemoryKind::Decision),
            other => Err(MemoryError::InvalidKind(other.to_string())),
        }
    }
}

impl std::fmt::Display for MemoryKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Onde uma memória vale.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Scope {
    /// Só no projeto dela.
    Project,
    /// Em todo projeto (memória geral de desenvolvimento do dono).
    Global,
}

impl Scope {
    pub fn as_str(&self) -> &'static str {
        match self {
            Scope::Project => "project",
            Scope::Global => "global",
        }
    }

    pub fn parse(s: &str) -> Scope {
        if s == "global" {
            Scope::Global
        } else {
            Scope::Project
        }
    }
}

/// Quem escreveu a memória.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Origin {
    /// O dono — suas regras vencem qualquer outra instrução.
    User,
    /// Uma IA do projeto (o orquestrador ou uma CLI), com o nome em `author`.
    Agent,
}

impl Origin {
    pub fn as_str(&self) -> &'static str {
        match self {
            Origin::User => "user",
            Origin::Agent => "agent",
        }
    }

    pub fn parse(s: &str) -> Origin {
        if s == "agent" {
            Origin::Agent
        } else {
            Origin::User
        }
    }
}

/// Uma memória persistida.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Memory {
    pub id: String,
    pub project: String,
    pub kind: MemoryKind,
    pub scope: Scope,
    pub origin: Origin,
    /// Nome de quem escreveu, quando foi uma IA (vazio para o dono).
    pub author: String,
    pub title: String,
    pub body: String,
    pub priority: i64,
    pub created_at: String,
    pub updated_at: String,
}

impl Memory {
    /// Rótulo curto de origem para índices: `global`, `dono` ou `IA: nome`.
    pub fn origin_label(&self) -> String {
        match (self.scope, self.origin) {
            (Scope::Global, _) => "global".to_string(),
            (_, Origin::User) => "dono".to_string(),
            (_, Origin::Agent) if self.author.trim().is_empty() => "IA".to_string(),
            (_, Origin::Agent) => format!("IA: {}", self.author),
        }
    }
}

/// Entrada do log de decisões (auditoria append-only).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DecisionEntry {
    pub id: String,
    pub project: String,
    pub session_id: String,
    pub action: String,
    pub decision: String,
    pub reason: String,
    pub created_at: String,
}

/// Memória de teste do dono, no projeto `p`.
#[cfg(test)]
pub(crate) fn test_memory(kind: MemoryKind, title: &str, body: &str, priority: i64) -> Memory {
    Memory {
        id: format!("id-{title}"),
        project: "p".into(),
        kind,
        scope: Scope::Project,
        origin: Origin::User,
        author: String::new(),
        title: title.into(),
        body: body.into(),
        priority,
        created_at: "2026-01-01".into(),
        updated_at: "2026-01-01".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    #[test]
    fn kind_roundtrip() {
        for k in MemoryKind::ALL {
            assert_eq!(MemoryKind::from_str(k.as_str()).unwrap(), k);
        }
        assert!(MemoryKind::from_str("banana").is_err());
    }

    #[test]
    fn scope_and_origin_roundtrip() {
        for s in [Scope::Project, Scope::Global] {
            assert_eq!(Scope::parse(s.as_str()), s);
        }
        for o in [Origin::User, Origin::Agent] {
            assert_eq!(Origin::parse(o.as_str()), o);
        }
    }

    #[test]
    fn origin_label_says_who_wrote_it() {
        let mut m = test_memory(MemoryKind::Practice, "t", "b", 0);
        assert_eq!(m.origin_label(), "dono");
        m.origin = Origin::Agent;
        m.author = "frontend".into();
        assert_eq!(m.origin_label(), "IA: frontend");
        m.author.clear();
        assert_eq!(m.origin_label(), "IA");
        m.scope = Scope::Global;
        assert_eq!(m.origin_label(), "global");
    }
}
