//! Sandbox de teste do orquestrador: um navegador e um sistema descartáveis
//! dentro de um container, para ele VERIFICAR o que as CLIs produziram.
//!
//! Três decisões de desenho, todas sobre custo de contexto:
//!
//! 1. **Texto primeiro.** Cada passo devolve a página como uma lista curta de
//!    elementos (`[e12] button "Salvar"`) e o texto visível — não pixels.
//!    Agir é `ui_click e12`, sem coordenadas.
//! 2. **Pixel sob demanda.** O screenshot existe, mas só quando pedido, e o
//!    PNG vai para um ARQUIVO: o orquestrador recebe o caminho e decide se
//!    vale abrir. Imagem nenhuma entra no contexto sem ele querer.
//! 3. **Só o que mudou.** Depois do primeiro snapshot, os seguintes mostram
//!    a diferença em relação ao anterior; a lista inteira só volta quando ele
//!    pedir. Toda resposta lembra o que ele pode chamar em seguida.

pub mod cdp;
pub mod container;
pub mod session;
pub mod view;

pub use container::{Mount, Sandbox, SandboxSpec};
pub use session::Session;
pub use view::{PageView, Snapshot};
