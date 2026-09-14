//! Verificação independente de um projeto web na sandbox.
//!
//! Sobe o backend DENTRO do container e exercita o frontend pela interface,
//! como uma pessoa faria: digitar, clicar, conferir o que mudou.
//!
//! `cargo run -p orchestrator-sandbox --example verificar -- <pasta> <arquivo.html>`
use orchestrator_sandbox::{container::SandboxSpec, view::Detail, Session};

fn main() -> anyhow::Result<()> {
    let dir = std::path::PathBuf::from(std::env::args().nth(1).unwrap());
    let pagina = std::env::args().nth(2).unwrap_or_else(|| "index.html".into());
    let mut spec = SandboxSpec::new("verificacao");
    spec.workdir = Some(dir.clone());
    // O backend grava o JSON de tarefas — e por padrão isso cai no
    // overlay descartável, sem alterar a pasta do projeto.
    let mut s = Session::start(&spec, &dir.join(".orchestrator/shots"))?;

    println!("== subindo o backend dentro da sandbox");
    println!(
        "{}",
        s.exec(&[
            "sh".into(),
            "-c".into(),
            "cd /work && (setsid python3 servidor.py > /tmp/srv.log 2>&1 &); sleep 3; \
             (curl -s --max-time 4 http://localhost:8080/api/tarefas || cat /tmp/srv.log) | head -3"
                .into(),
        ])?
    );

    println!("== abrindo a página");
    println!("{}", s.open(&format!("/work/{pagina}"))?);

    // Descobre o campo de texto e o botão a partir do que a página expôs.
    let snap = s.snapshot(Detail::Full)?;
    println!("== elementos:\n{snap}");
    let campo = primeiro(&snap, &["input:text", "input:", "textarea"]);
    let botao = primeiro(&snap, &["button"]);

    if let (Some(campo), Some(botao)) = (&campo, &botao) {
        println!("== digitando em {campo} e clicando em {botao}");
        println!("{}", s.act("type", campo, "tarefa criada pelo teste")?);
        println!("{}", s.act("click", botao, "")?);
    } else {
        println!("!! não achei campo/botão para interagir");
    }

    // Ciclo completo: concluir e apagar o que acabamos de criar.
    let depois = s.snapshot(Detail::Full)?;
    if let Some(caixa) = primeiro(&depois, &["input:checkbox"]) {
        println!("== marcando como concluída ({caixa})");
        println!("{}", s.act("click", &caixa, "")?);
    }
    let antes_apagar = s.snapshot(Detail::Full)?;
    if let Some(apagar) = ultimo(&antes_apagar, &["button"]) {
        println!("== apagando ({apagar})");
        println!("{}", s.act("click", &apagar, "")?);
    }

    let shot = s.screenshot()?;
    println!("== screenshot: {}", shot.display());
    s.stop()?;
    Ok(())
}

/// Última referência cujo papel casa um dos prefixos dados.
fn ultimo(render: &str, papeis: &[&str]) -> Option<String> {
    let mut achado = None;
    for linha in render.lines() {
        let linha = linha.trim();
        if !linha.starts_with('[') {
            continue;
        }
        if let Some((r, resto)) = linha[1..].split_once("] ") {
            if papeis.iter().any(|p| resto.starts_with(p)) {
                achado = Some(r.to_string());
            }
        }
    }
    achado
}

/// Primeira referência cujo papel casa um dos prefixos dados.
fn primeiro(render: &str, papeis: &[&str]) -> Option<String> {
    for linha in render.lines() {
        let linha = linha.trim();
        if !linha.starts_with('[') {
            continue;
        }
        let (r, resto) = linha[1..].split_once("] ")?;
        if papeis.iter().any(|p| resto.starts_with(p)) {
            return Some(r.to_string());
        }
    }
    None
}
