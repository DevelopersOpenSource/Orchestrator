//! Exercita a sandbox de ponta a ponta contra o container REAL.
//!
//! Não roda na suíte (precisa da imagem construída); é o roteiro de
//! verificação manual:
//!
//! ```sh
//! podman build -t orchestrator-sandbox -f packaging/sandbox/Containerfile .
//! cargo run -p orchestrator-sandbox --example live -- /uma/pasta/com/pagina.html
//! ```
fn main() -> anyhow::Result<()> {
    use orchestrator_sandbox::{container::SandboxSpec, Session};
    let dir = std::path::PathBuf::from(std::env::args().nth(1).unwrap());
    let mut spec = SandboxSpec::new("verificacao");
    spec.workdir = Some(dir.clone());
    let mut s = Session::start(&spec, &dir.join("shots"))?;
    println!("=== ui_open:\n{}", s.open("/work/pagina.html")?);
    println!("=== ui_type e2:\n{}", s.act("type", "e1", "eu@exemplo.com")?);
    println!("=== ui_type num BOTÃO (erro esperado):\n{}", s.act("type", "e2", "x")?);
    println!("=== ui_click e2:\n{}", s.act("click", "e2", "")?);
    println!("=== ui_exec:\n{}", s.exec(&["sh".into(), "-c".into(), "echo rodando dentro; uname -s".into()])?);
    let shot = s.screenshot()?;
    println!("=== ui_screenshot: {} ({} bytes)", shot.display(), std::fs::metadata(&shot)?.len());
    s.stop()?;
    Ok(())
}
