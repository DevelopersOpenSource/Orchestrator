//! `orchestrator-memoryd` — o serviço de memória semântica do Orchestrator.
//!
//! É o ÚNICO processo que carrega modelos e fala com o ChromaDB. Todo o
//! resto (hook, MCP, CLI, TUI) pede por um socket unix, com prazo, e cai no
//! ranqueamento léxico do SQLite se ele não responder.
//!
//! - [`models`]: embedding multilíngue (E5) e reranker cross-encoder;
//! - [`chroma`]: o container do ChromaDB e o índice vetorial;
//! - [`download`]: os modelos, baixados na 1ª vez com checksum;
//! - [`engine`]: a busca híbrida e a indexação;
//! - [`http`]: a API e a página em 127.0.0.1:10000;
//! - [`server`]: o socket.

pub mod chroma;
pub mod download;
pub mod engine;
pub mod http;
pub mod models;
pub mod server;
