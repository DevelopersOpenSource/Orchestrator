//! Exercita os scripts que rodam DENTRO da página (`page.js`/`action.js`).
//!
//! Eles são a peça central da sandbox — é o que substitui o screenshot pela
//! lista de elementos — e não dá para testá-los em Rust. O teste roda o
//! `node` com um DOM falso; sem `node` instalado, ele é pulado em vez de
//! falhar (não é dependência do produto, só da verificação).

use std::path::PathBuf;
use std::process::Command;

#[test]
fn page_and_action_scripts_work_against_a_fake_dom() {
    if Command::new("node").arg("--version").output().is_err() {
        eprintln!("pulando: `node` não está instalado");
        return;
    }
    let repo = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .expect("raiz do workspace")
        .to_path_buf();
    let script = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/page_js.mjs");

    let out = Command::new("node")
        .arg(&script)
        .current_dir(&repo)
        .output()
        .expect("rodando node");

    assert!(
        out.status.success(),
        "os scripts da página falharam:\n{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("elementos lidos"), "{stdout}");
}
