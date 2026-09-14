//! `orchestrator-engine` — o núcleo do Orchestrator, sem interface.
//!
//! A TUI (ratatui) e o app desktop (Tauri) desenham por cima dele: os dois
//! abrem as mesmas CLIs reais, conversam com o mesmo orquestrador e executam
//! os mesmos comandos `/`.
//!
//! - [`term`]: CLI real num PTY — estado, entrega de prompt, histórico, menus;
//! - [`chat`]: o chat do orquestrador (CLI `claude` ou API compatível);
//! - [`agent_card`]: agente headless dirigido pelo orquestrador;
//! - [`palette`]: os comandos `/` e o estado de cada um;
//! - [`picker_dir`]: o seletor de pastas nativo do sistema;
//! - [`providers`]: quem pode responder no chat agora, e o que falta;
//! - [`workbench`]: o [`Engine`] — projetos, workspaces, cards e a fila do MCP.

pub mod agent_card;
pub mod chat;
pub mod mcp_client;
pub mod palette;
pub mod picker_dir;
pub mod providers;
pub mod term;
pub mod workbench;

pub use workbench::{Engine, EngineEvent};
