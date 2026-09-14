//! Exercita a leitura e a escolha de menu contra um questionário REAL.
//!
//! `cargo run -p orchestrator-tui --example menu_vivo -- <script.py> [opção] [resposta]`
fn main() -> anyhow::Result<()> {
    let script = std::env::args().nth(1).unwrap();
    let opcao = std::env::args().nth(2).unwrap_or_else(|| "Não usar".into());
    let resposta = std::env::args().nth(3);
    let mut t = orchestrator_tui::term::TermSession::spawn_env(
        "questionario".into(),
        "python3",
        &[script],
        std::path::Path::new("/tmp"),
        24,
        80,
        &[],
    )?;
    t.managed = true;
    std::thread::sleep(std::time::Duration::from_millis(1200));

    let menu = t
        .menu()
        .ok_or_else(|| anyhow::anyhow!("não vi menu.\n{}", t.screen_text()))?;
    println!("PERGUNTA: {}", menu.question);
    for (i, o) in menu.options.iter().enumerate() {
        println!(
            "{} {}) {}{}",
            if o.selected { "▶" } else { " " },
            i + 1,
            o.text,
            if o.free_text { "  [resposta escrita]" } else { "" }
        );
    }
    println!("\n=> escolhendo {opcao:?}...");
    println!("{}", t.choose(&opcao, resposta.as_deref())?);
    std::thread::sleep(std::time::Duration::from_millis(2500));
    println!("\nTELA DEPOIS:\n{}", t.screen_tail(8));
    t.kill();
    Ok(())
}
