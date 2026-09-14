//! `orchestrator-core` — tipos e lógica compartilhada do Orchestrator.
//!
//! Este crate não faz IO de rede nem fala com agentes: apenas define a
//! configuração, o estado das sessões de projeto e os erros comuns.

pub mod config;
pub mod detect;
pub mod error;
pub mod project;

pub use config::{
    AgentDefaults, CliSpec, Config, LlmBackend, LlmProvider, ProjectConfig, ProviderKind,
};
pub use error::CoreError;
pub use project::{Project, SessionState};
