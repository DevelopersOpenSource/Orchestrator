//! # orchestrator-cli-adapter
//!
//! Controle programático direto de agentes de código CLI (Fase 2).
//!
//! Este crate NUNCA usa automação de teclado: toda a integração acontece
//! via subprocesso + JSON pela stdio (`--output-format stream-json`).

pub mod agent;
pub mod antigravity;
pub mod claude_code;
pub mod codex;
pub mod harness;
pub mod kimi;
pub mod opencode;

pub use agent::{AgentError, CliAgent, SessionHandle};
pub use claude_code::capabilities::{discover, CliCapabilities};
pub use claude_code::stream::{collect_result, StreamEvent, TurnResult};
pub use claude_code::{AgentOptions, ClaudeCodeAgent};
